//! Token counting for budget pipeline and savings telemetry.
//!
//! Two modes are supported:
//!
//! - [`Tokenizer::Heuristic`] — fast `chars / 3.5` approximation, ±20%
//!   accuracy. Used for budget enforcement, where over-/under-shoot of a
//!   few percent is absorbed by the 20% budget margin.
//!
//! - [`Tokenizer::Cl100kBase`] / [`Tokenizer::O200kBase`] — exact BPE
//!   counts via `tiktoken-rs`. Slower (1–10 µs per call after warm-up,
//!   plus ~5 ms one-time table load) but reproducible across host and
//!   eval scripts. Required for the §Savings Accounting reporting rule
//!   in Paper 2 — quoted savings numbers are tokenizer-dependent and
//!   must be computed against a named tokenizer.
//!
//! The default tokenizer for new code is [`Tokenizer::Heuristic`] so that
//! existing callers (budget pipeline, doc examples) keep their fast path.
//! Telemetry / `tune` should explicitly pick a BPE variant.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tiktoken_rs::CoreBPE;

/// Average chars/token ratio for structured data.
///
/// For Claude/GPT-4:
/// - English text: ~4 chars/token
/// - Code/structured data: ~3.5 chars/token
/// - TOON format: ~3.5 chars/token (short keys, dense data)
///
/// We use 3.5 for a conservative estimate (better to overestimate than underestimate).
const CHARS_PER_TOKEN: f64 = 3.5;

/// Estimate the number of tokens in text via the heuristic.
///
/// Returns an approximate token count.
/// Accuracy: +/-20%, which is covered by the margin in the budget pipeline.
///
/// Equivalent to `Tokenizer::Heuristic.count(text)`.
pub fn estimate_tokens(text: &str) -> usize {
    Tokenizer::Heuristic.count(text)
}

/// Estimate the number of characters for a given token budget.
///
/// Inverse operation of `estimate_tokens` for the heuristic only.
pub fn tokens_to_chars(tokens: usize) -> usize {
    (tokens as f64 * CHARS_PER_TOKEN).floor() as usize
}

/// Tokenizer choice for token counting.
///
/// `Heuristic` is the fast char-based approximation. The `*Base` variants
/// load a real BPE table once and produce exact counts that match the
/// model provider's billing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Tokenizer {
    /// `chars / 3.5` approximation. ±20% accuracy.
    #[default]
    Heuristic,
    /// `cl100k_base`: GPT-3.5-turbo / GPT-4 / Claude (older).
    Cl100kBase,
    /// `o200k_base`: GPT-4o / GPT-4o-mini / Claude (current Anthropic
    /// models reportedly share this family on the o200k axis).
    O200kBase,
}

impl Tokenizer {
    /// Count tokens in `text` using the selected tokenizer.
    ///
    /// On failure to load a BPE table at first use, the variant
    /// degrades to [`Self::Heuristic`] silently (with a one-time
    /// `WARN` log). Better an over-conservative estimate than a panic
    /// in the MCP hot path.
    pub fn count(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        match self {
            Self::Heuristic => (text.len() as f64 / CHARS_PER_TOKEN).ceil() as usize,
            Self::Cl100kBase => match cl100k_bpe() {
                Some(bpe) => bpe.encode_with_special_tokens(text).len(),
                None => Self::Heuristic.count(text),
            },
            Self::O200kBase => match o200k_bpe() {
                Some(bpe) => bpe.encode_with_special_tokens(text).len(),
                None => Self::Heuristic.count(text),
            },
        }
    }

    /// Stable identifier for telemetry / config serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Heuristic => "heuristic",
            Self::Cl100kBase => "cl100k_base",
            Self::O200kBase => "o200k_base",
        }
    }

    /// Parse from telemetry / config string. Unknown values fall back to
    /// `Heuristic` to keep startup non-fatal on typos.
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "cl100k_base" | "cl100k" => Self::Cl100kBase,
            "o200k_base" | "o200k" => Self::O200kBase,
            _ => Self::Heuristic,
        }
    }
}

/// Lazily-loaded BPE tables. Returning `Option` (instead of expecting
/// the load to succeed) means a tiktoken-rs build/feature regression
/// degrades token counting to the heuristic instead of panicking the
/// MCP server. Logs a single WARN per BPE family on first failure.
fn cl100k_bpe() -> Option<&'static CoreBPE> {
    static BPE: OnceLock<Option<CoreBPE>> = OnceLock::new();
    BPE.get_or_init(|| match tiktoken_rs::cl100k_base() {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(
                target: "devboy_format_pipeline::tokenizer",
                "cl100k_base BPE table failed to load: {e} — \
                 falling back to chars/3.5 heuristic"
            );
            None
        }
    })
    .as_ref()
}

fn o200k_bpe() -> Option<&'static CoreBPE> {
    static BPE: OnceLock<Option<CoreBPE>> = OnceLock::new();
    BPE.get_or_init(|| match tiktoken_rs::o200k_base() {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(
                target: "devboy_format_pipeline::tokenizer",
                "o200k_base BPE table failed to load: {e} — \
                 falling back to chars/3.5 heuristic"
            );
            None
        }
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(Tokenizer::Cl100kBase.count(""), 0);
        assert_eq!(Tokenizer::O200kBase.count(""), 0);
    }

    #[test]
    fn test_short_text() {
        // "hello" = 5 chars → ceil(5/3.5) = 2 tokens
        assert_eq!(estimate_tokens("hello"), 2);
    }

    #[test]
    fn test_structured_data() {
        let toon = "key: gh#1\ntitle: Fix bug\nstate: open";
        let tokens = estimate_tokens(toon);
        // 37 chars / 3.5 ≈ 10.6 → 11 tokens
        assert_eq!(tokens, 11);
    }

    #[test]
    fn test_round_trip() {
        let budget = 8000;
        let chars = tokens_to_chars(budget);
        let back = estimate_tokens(&"x".repeat(chars));
        // Should be approximately equal to the budget (±1 due to rounding)
        assert!((back as i64 - budget as i64).unsigned_abs() <= 1);
    }

    #[test]
    fn test_tokens_to_chars() {
        // 8000 tokens × 3.5 = 28000 chars
        assert_eq!(tokens_to_chars(8000), 28000);
    }

    #[test]
    fn cl100k_and_o200k_produce_positive_counts_on_simple_input() {
        // Stable invariants only — exact token counts can shift across
        // tiktoken-rs upgrades even when the BPE family name does not.
        // We assert (a) the counter returns a positive value and (b) it
        // is at least as compact as the chars/3.5 heuristic on this
        // English phrase. Both are properties any sensible BPE shares.
        let phrase = "hello world";
        let heuristic = Tokenizer::Heuristic.count(phrase);
        for tk in [Tokenizer::Cl100kBase, Tokenizer::O200kBase] {
            let n = tk.count(phrase);
            assert!(n > 0, "{tk:?} returned zero on `{phrase}`");
            assert!(
                n <= heuristic,
                "{tk:?} reported {n} tokens, worse than the {heuristic}-token heuristic prior"
            );
        }
    }

    #[test]
    fn cl100k_and_o200k_agree_on_hello_world() {
        // Common-English short phrases tokenize identically across the
        // two BPE families. This is a tighter — but still vocabulary-
        // version-tolerant — invariant than pinning the exact count.
        let cl = Tokenizer::Cl100kBase.count("hello world");
        let o2 = Tokenizer::O200kBase.count("hello world");
        assert_eq!(cl, o2, "cl100k and o200k should agree on `hello world`");
    }

    #[test]
    fn cl100k_and_o200k_disagree_on_jsonish() {
        // The two tokenizers diverge on JSON-shaped input — the headline
        // observation behind §Savings Accounting. We don't pin exact
        // numbers here (they may shift with tiktoken-rs upgrades), only
        // the inequality.
        let json = "{\"id\":42,\"name\":\"alpha\",\"tags\":[\"x\",\"y\",\"z\"]}";
        let cl = Tokenizer::Cl100kBase.count(json);
        let o2 = Tokenizer::O200kBase.count(json);
        assert!(cl > 0 && o2 > 0);
        assert_ne!(cl, o2);
    }

    #[test]
    fn heuristic_default_is_heuristic() {
        assert_eq!(Tokenizer::default(), Tokenizer::Heuristic);
        assert_eq!(Tokenizer::default().as_str(), "heuristic");
    }

    #[test]
    fn from_str_lossy_known_and_unknown() {
        assert_eq!(
            Tokenizer::from_str_lossy("cl100k_base"),
            Tokenizer::Cl100kBase
        );
        assert_eq!(Tokenizer::from_str_lossy("CL100K"), Tokenizer::Cl100kBase);
        assert_eq!(
            Tokenizer::from_str_lossy("o200k_base"),
            Tokenizer::O200kBase
        );
        assert_eq!(Tokenizer::from_str_lossy("o200k"), Tokenizer::O200kBase);
        assert_eq!(Tokenizer::from_str_lossy("nonsense"), Tokenizer::Heuristic);
        assert_eq!(Tokenizer::from_str_lossy(""), Tokenizer::Heuristic);
    }

    #[test]
    fn round_trip_serde() {
        for tk in [
            Tokenizer::Heuristic,
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
        ] {
            let json = serde_json::to_string(&tk).unwrap();
            let back: Tokenizer = serde_json::from_str(&json).unwrap();
            assert_eq!(tk, back);
            // Round-trip via stable string id used by config / telemetry.
            assert_eq!(Tokenizer::from_str_lossy(tk.as_str()), tk);
        }
    }
}
