//! Approximation of token count in text.
//!
//! Uses char-based estimation instead of tiktoken-rs to save ~2MB binary size.
//! A 20% margin in the budget pipeline compensates for estimation inaccuracy.

/// Average chars/token ratio for structured data.
///
/// For Claude/GPT-4:
/// - English text: ~4 chars/token
/// - Code/structured data: ~3.5 chars/token
/// - TOON format: ~3.5 chars/token (short keys, dense data)
///
/// We use 3.5 for a conservative estimate (better to overestimate than underestimate).
const CHARS_PER_TOKEN: f64 = 3.5;

/// Estimate the number of tokens in text.
///
/// Returns an approximate token count.
/// Accuracy: +/-20%, which is covered by the margin in the budget pipeline.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    (text.len() as f64 / CHARS_PER_TOKEN).ceil() as usize
}

/// Estimate the number of characters for a given token budget.
///
/// Inverse operation of `estimate_tokens`.
pub fn tokens_to_chars(tokens: usize) -> usize {
    (tokens as f64 * CHARS_PER_TOKEN).floor() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
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
}
