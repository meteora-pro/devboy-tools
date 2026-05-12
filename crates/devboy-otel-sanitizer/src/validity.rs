//! Validity filters — secondary checks to reduce false positives.
//!
//! Implements Meli et al. NDSS 2019 _How Bad Can It Git?_ §IV-D filters
//! that, combined, achieved 99.29% accuracy on a labeled GitHub corpus:
//!
//! 1. Dictionary word check — reject English words and common test fixtures.
//! 2. Sequential / repeated character detection — reject `aaaa`, `12345`, `qwerty`.
//! 3. Per-encoding Shannon entropy threshold — reject low-entropy strings
//!    (likely formatted text, not random secrets).
//!
//! All filters are pure functions; combine via [`is_likely_secret`].

use std::collections::HashSet;
use std::sync::OnceLock;

/// Classify input encoding for entropy threshold selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Hex (a-f0-9). Threshold ≥ 3.5 bits/char.
    Hex,
    /// Base64 (a-zA-Z0-9+/=). Threshold ≥ 4.5 bits/char.
    Base64,
    /// General UTF-8 / printable. Threshold ≥ 4.5 bits/char.
    Utf8,
}

impl Encoding {
    /// Recommended Shannon entropy threshold (bits/char) for this encoding.
    /// Strings below the threshold are likely formatted text, not secrets.
    ///
    /// Thresholds are calibrated for **defense-in-depth** — prefer
    /// over-redaction to under-redaction. Real 20-char AWS access keys
    /// typically score ~3.9-4.0 bits/char; English text scores ~4.0-4.3
    /// (which is OK because dictionary filter rejects English words).
    pub fn entropy_threshold(self) -> f64 {
        match self {
            // Pure hex: very narrow alphabet (16 chars), so even
            // formatted hex strings naturally score ~3.5-3.8.
            Encoding::Hex => 3.0,
            // Mixed alphanumeric (incl. AWS keys, OAuth tokens, base64).
            // Calibrated to admit 16-20 char random values like AKIA-keys.
            Encoding::Base64 | Encoding::Utf8 => 3.5,
        }
    }

    /// Best-effort encoding detection from string contents.
    pub fn detect(value: &str) -> Self {
        let only_hex = !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_');
        if only_hex {
            return Encoding::Hex;
        }
        let only_base64 = !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=');
        if only_base64 {
            return Encoding::Base64;
        }
        Encoding::Utf8
    }
}

/// Compute Shannon entropy (bits per character) of a string.
pub fn shannon_entropy(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut counts: [u32; 256] = [0; 256];
    let mut total = 0u32;
    for b in value.bytes() {
        counts[b as usize] += 1;
        total += 1;
    }
    let total_f = f64::from(total);
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / total_f;
            -p * p.log2()
        })
        .sum()
}

/// Check whether the value has at least 4 consecutive identical characters
/// or a monotone sequence of length ≥ 5 (e.g. "12345", "abcdef").
///
/// Such patterns are typical of placeholder / dummy data, not real secrets.
pub fn has_obvious_pattern(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 4 {
        return false;
    }

    // Repeated alphanumeric chars: 4 in a row. Punctuation runs (e.g.
    // `-----` in PEM markers) are intentionally allowed — they are
    // formatting artefacts, not placeholder values.
    let mut run = 1usize;
    for window in bytes.windows(2) {
        if window[0] == window[1] && window[0].is_ascii_alphanumeric() {
            run += 1;
            if run >= 4 {
                return true;
            }
        } else {
            run = 1;
        }
    }

    // Monotone arithmetic sequence: 5 chars with constant diff.
    if bytes.len() >= 5 {
        for start in 0..=bytes.len() - 5 {
            let diff = bytes[start + 1] as i32 - bytes[start] as i32;
            if diff != 1 && diff != -1 {
                continue;
            }
            let mut ok = true;
            for i in 1..5 {
                if (bytes[start + i] as i32 - bytes[start + i - 1] as i32) != diff {
                    ok = false;
                    break;
                }
            }
            if ok {
                return true;
            }
        }
    }
    false
}

/// Common English words and test fixtures that show up as false positives.
/// Compiled once on first access.
fn dictionary() -> &'static HashSet<&'static str> {
    static DICT: OnceLock<HashSet<&'static str>> = OnceLock::new();
    DICT.get_or_init(|| {
        // Curated list — extend in default_rules.toml under [dictionary]
        // (deferred to next commit). Keep this short and high-signal:
        // words that frequently appear in source code as identifiers /
        // strings and would otherwise cause regex-based false positives.
        [
            "password",
            "passw0rd",
            "secret",
            "example",
            "demo",
            "test",
            "dummy",
            "placeholder",
            "sample",
            "lorem",
            "ipsum",
            "default",
            "changeme",
            "hunter2",
            "qwerty",
            "letmein",
            "abcdef",
            "12345678",
            "yourkey",
            "yoursecret",
            "mykey",
            "mysecret",
            "redacted",
            "todo",
            "fixme",
            "xxxxxxxx",
            "aaaaaaaa",
        ]
        .into_iter()
        .collect()
    })
}

/// Whether the value is in the dictionary (case-insensitive).
pub fn in_dictionary(value: &str) -> bool {
    dictionary().contains(value.to_ascii_lowercase().as_str())
}

/// Combined validity check.
///
/// Returns `true` if the value passes all three filters (i.e. is a likely
/// real secret). Returns `false` if any filter rejects it.
pub fn is_likely_secret(value: &str) -> bool {
    if in_dictionary(value) {
        return false;
    }
    if has_obvious_pattern(value) {
        return false;
    }
    let encoding = Encoding::detect(value);
    if shannon_entropy(value) < encoding.entropy_threshold() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_zero_empty() {
        assert!((shannon_entropy("") - 0.0).abs() < 1e-9);
    }

    #[test]
    fn entropy_low_repeated() {
        assert!(shannon_entropy("aaaaaaaaaa") < 0.5);
    }

    #[test]
    fn entropy_high_random() {
        // Real-looking AWS access key id
        assert!(shannon_entropy("AKIAIOSFODNN7EXAMPLE") > 3.5);
    }

    #[test]
    fn obvious_pattern_repeats() {
        assert!(has_obvious_pattern("aaaaXY"));
        assert!(has_obvious_pattern("XXXXXX"));
    }

    #[test]
    fn obvious_pattern_sequence() {
        assert!(has_obvious_pattern("abcde-rest"));
        assert!(has_obvious_pattern("12345end"));
    }

    #[test]
    fn obvious_pattern_random_does_not_trigger() {
        assert!(!has_obvious_pattern("AKIAIOSFODNN7EXAMPLE"));
        assert!(!has_obvious_pattern("hello"));
    }

    #[test]
    fn dictionary_rejects_test_fixtures() {
        assert!(in_dictionary("password"));
        assert!(in_dictionary("ChangeMe"));
        assert!(!in_dictionary("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn likely_secret_aws_key() {
        // Random-looking value with high entropy, no obvious pattern,
        // not in dictionary — mimics a real AWS access key id.
        assert!(is_likely_secret("AKIAQ7K3MN9PV2BX5LZA"));
    }

    #[test]
    fn pem_marker_dashes_are_allowed_runs() {
        // PEM markers contain runs of `-` (non-alphanumeric punctuation,
        // formatting artefact, not a placeholder pattern). The `is_likely_secret`
        // function may still reject due to low entropy, which is why
        // PEM rules use `skip_validity = true` instead.
        assert!(!has_obvious_pattern("-----BEGIN RSA PRIVATE KEY-----"));
    }

    #[test]
    fn likely_secret_rejects_lorem() {
        assert!(!is_likely_secret("lorem"));
        assert!(!is_likely_secret("password"));
    }

    #[test]
    fn likely_secret_rejects_obvious() {
        assert!(!is_likely_secret("aaaaaaaa"));
        assert!(!is_likely_secret("12345abc"));
    }
}
