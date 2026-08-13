//! Reverse scrub: turn secret **values** back into their aliases
//! (ADR-024 §4).
//!
//! # Why this runs server-side
//!
//! The audit log exists to record what really happened, including
//! an agent's mistakes. Asking the agent to scrub its own text
//! before sending would make that an honour system — precisely the
//! thing the log is supposed to be evidence against. So the scrub
//! runs inside the daemon's write path, before encryption, and the
//! resulting property is structural rather than conventional: an
//! agent **cannot** cause a raw value to be persisted, because the
//! code that persists it replaces values first.
//!
//! # Two kinds of match
//!
//! - **Known value → `@secret:<path>`.** The daemon holds every
//!   provisioned value while unlocked, so it can match on the value
//!   itself and say *which* secret leaked. Keeping the path is
//!   deliberate: "a GitLab deploy token appeared in this log line"
//!   is debuggable, while a bare `[REDACTED]` is not.
//! - **Unknown but secret-shaped → `[REDACTED:<pattern_id>]`.**
//!   Anything matching a catalogue pattern is redacted even though
//!   no path is known, which catches tokens the framework never
//!   provisioned.
//!
//! # Which regex the shape pass uses
//!
//! Catalogue patterns are anchored (`^glpat-…$`) because their first
//! job is validating a whole candidate value. Fed to this pass as-is
//! they can only match when the text *is* a single token, so the
//! embedded occurrences the pass exists for slip through — which is
//! exactly what happened until [`crate::scanning`] was added.
//!
//! So the pass prefers [`SecretPattern::scan_regex`] and falls back to
//! the anchored [`SecretPattern::format_regex`] only for patterns that
//! cannot safely scan free text. Those keep whole-string matching:
//! narrower than before is wrong, and scanning them would be worse
//! than not scanning at all. [`Scrubber::scanning_pattern_count`]
//! reports the split so a caller can say which protection it has.
//!
//! # Cost
//!
//! Matching uses an Aho-Corasick automaton built once from the
//! value set, so a scrub is O(text length) regardless of how many
//! secrets exist. That matters because it runs on every write.

use aho_corasick::{AhoCorasick, MatchKind};

use crate::SecretPattern;

/// Prefix used when a value is replaced by its path.
pub const ALIAS_PREFIX: &str = "@secret:";

/// One replacement the scrub performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// The secret's ADR-020 path, or the pattern id for a
    /// shape-only match.
    pub label: String,
    /// Whether the value was a known provisioned secret.
    pub known: bool,
    /// How many occurrences were replaced.
    pub count: usize,
}

/// Result of scrubbing one piece of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubOutcome {
    /// The text with every recognised value replaced.
    pub text: String,
    /// What was replaced, for the leak-audit entry. Never carries
    /// the value itself.
    pub replacements: Vec<Replacement>,
}

impl ScrubOutcome {
    /// Whether anything was replaced.
    pub fn had_leak(&self) -> bool {
        !self.replacements.is_empty()
    }
}

/// One catalogue pattern prepared for the shape pass.
struct PatternMatcher {
    /// Pattern id, used as the redaction label.
    id: String,
    /// The regex actually applied — scanning form where the pattern
    /// has one, anchored validator otherwise.
    regex: regex::Regex,
    /// Whether `regex` can match inside a larger string. `false`
    /// means this pattern only fires on text that is entirely one
    /// token.
    scans: bool,
}

/// Matches provisioned values, and secret-shaped strings that are
/// not provisioned.
pub struct Scrubber {
    /// Automaton over the provisioned values.
    automaton: Option<AhoCorasick>,
    /// Path for each pattern index in the automaton.
    paths: Vec<String>,
    /// Catalogue patterns used for the shape-only fallback.
    patterns: Vec<PatternMatcher>,
}

impl std::fmt::Debug for Scrubber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the values themselves.
        f.debug_struct("Scrubber")
            .field("known_values", &self.paths.len())
            .field("patterns", &self.patterns.len())
            .finish()
    }
}

impl Scrubber {
    /// Build a scrubber over `(path, value)` pairs.
    ///
    /// Empty and very short values are skipped: a one-character
    /// "secret" would match everywhere and turn the log into
    /// confetti, which is worse than not scrubbing it.
    pub fn new<I, P, V>(values: I) -> Self
    where
        I: IntoIterator<Item = (P, V)>,
        P: Into<String>,
        V: AsRef<str>,
    {
        const MIN_MATCHABLE_LEN: usize = 8;

        let mut paths = Vec::new();
        let mut needles = Vec::new();

        for (path, value) in values {
            let value = value.as_ref();
            if value.len() < MIN_MATCHABLE_LEN {
                continue;
            }
            paths.push(path.into());
            needles.push(value.to_owned());
        }

        let automaton = if needles.is_empty() {
            None
        } else {
            AhoCorasick::builder()
                // Longest match wins, so a value that contains
                // another value is replaced whole rather than in
                // pieces.
                .match_kind(MatchKind::LeftmostLongest)
                .build(&needles)
                .ok()
        };

        Self {
            automaton,
            paths,
            patterns: Vec::new(),
        }
    }

    /// Add catalogue patterns for the shape-only fallback.
    pub fn with_patterns<'a, I>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = &'a dyn SecretPattern>,
    {
        for pattern in patterns {
            // Scanning form first: the anchored validator cannot find
            // a token inside a sentence, which is the only case this
            // pass is here for.
            let (regex, scans) = match pattern.scan_regex() {
                Some(scan) => (scan.clone(), true),
                None => (pattern.format_regex().clone(), false),
            };
            self.patterns.push(PatternMatcher {
                id: pattern.id().to_owned(),
                regex,
                scans,
            });
        }
        self
    }

    /// Replace every recognised value in `text`.
    pub fn scrub(&self, text: &str) -> ScrubOutcome {
        let (text, mut replacements) = self.scrub_known(text);
        let (text, pattern_hits) = self.scrub_shapes(&text);
        replacements.extend(pattern_hits);

        ScrubOutcome { text, replacements }
    }

    /// Pass one: known provisioned values → `@secret:<path>`.
    fn scrub_known(&self, text: &str) -> (String, Vec<Replacement>) {
        let Some(automaton) = &self.automaton else {
            return (text.to_owned(), Vec::new());
        };

        let mut counts = vec![0usize; self.paths.len()];
        let mut out = String::with_capacity(text.len());
        let mut last = 0;

        for m in automaton.find_iter(text) {
            out.push_str(&text[last..m.start()]);
            let idx = m.pattern().as_usize();
            out.push_str(ALIAS_PREFIX);
            out.push_str(&self.paths[idx]);
            counts[idx] += 1;
            last = m.end();
        }
        out.push_str(&text[last..]);

        let replacements = counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(idx, count)| Replacement {
                label: self.paths[idx].clone(),
                known: true,
                count: *count,
            })
            .collect();

        (out, replacements)
    }

    /// Pass two: unprovisioned but secret-shaped strings →
    /// `[REDACTED:<pattern_id>]`.
    fn scrub_shapes(&self, text: &str) -> (String, Vec<Replacement>) {
        let mut out = text.to_owned();
        let mut replacements = Vec::new();

        for matcher in &self.patterns {
            let count = matcher.regex.find_iter(&out).count();
            if count == 0 {
                continue;
            }
            out = matcher
                .regex
                .replace_all(&out, format!("[REDACTED:{}]", matcher.id).as_str())
                .into_owned();
            replacements.push(Replacement {
                label: matcher.id.clone(),
                known: false,
                count,
            });
        }

        (out, replacements)
    }

    /// How many provisioned values this scrubber can recognise.
    pub fn known_value_count(&self) -> usize {
        self.paths.len()
    }

    /// How many catalogue patterns can find a match *inside* a larger
    /// string, out of [`Scrubber::pattern_count`] total.
    ///
    /// The remainder only fire on text that is entirely one token.
    /// Worth surfacing rather than assuming: a caller that reports
    /// "shape-based redaction is on" while every pattern is
    /// whole-string-only is describing protection it does not have.
    pub fn scanning_pattern_count(&self) -> usize {
        self.patterns.iter().filter(|p| p.scans).count()
    }

    /// How many catalogue patterns are loaded.
    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    struct TestPattern {
        id: &'static str,
        regex: Regex,
    }

    impl SecretPattern for TestPattern {
        fn id(&self) -> &str {
            self.id
        }
        fn display_name(&self) -> &str {
            self.id
        }
        fn format_regex(&self) -> &Regex {
            &self.regex
        }
        fn severity(&self) -> crate::Severity {
            crate::Severity::High
        }
    }

    fn gitlab_pattern() -> TestPattern {
        TestPattern {
            id: "gitlab-pat",
            regex: Regex::new(r"glpat-[A-Za-z0-9_-]{20}").unwrap(),
        }
    }

    /// The property the whole module exists for.
    #[test]
    fn a_known_value_is_replaced_by_its_path() {
        let s = Scrubber::new([("team/gitlab/token", "glpat-REALTOKENVALUE1234")]);
        let out = s.scrub("deploy failed using glpat-REALTOKENVALUE1234 as the token");

        assert!(
            !out.text.contains("glpat-REALTOKENVALUE1234"),
            "the value survived: {}",
            out.text
        );
        assert!(
            out.text.contains("@secret:team/gitlab/token"),
            "{}",
            out.text
        );
        assert_eq!(out.replacements.len(), 1);
        assert!(out.replacements[0].known);
    }

    /// Keeping the path is deliberate: knowing *which* secret
    /// leaked is what makes the log debuggable.
    #[test]
    fn replacements_name_the_secret_but_never_the_value() {
        let s = Scrubber::new([("team/gitlab/token", "glpat-REALTOKENVALUE1234")]);
        let out = s.scrub("glpat-REALTOKENVALUE1234");

        let rendered = format!("{:?}", out.replacements);
        assert!(rendered.contains("team/gitlab/token"));
        assert!(!rendered.contains("REALTOKENVALUE"), "{rendered}");
    }

    #[test]
    fn every_occurrence_is_replaced_and_counted() {
        let s = Scrubber::new([("p", "SECRETVALUE123")]);
        let out = s.scrub("SECRETVALUE123 then SECRETVALUE123 again");

        assert!(!out.text.contains("SECRETVALUE123"));
        assert_eq!(out.replacements[0].count, 2);
    }

    /// An unprovisioned but secret-shaped token is still caught,
    /// just without a path to name.
    #[test]
    fn an_unknown_but_shaped_token_is_redacted_by_pattern() {
        let pattern = gitlab_pattern();
        let s = Scrubber::new(Vec::<(String, String)>::new())
            .with_patterns([&pattern as &dyn SecretPattern]);

        let out = s.scrub("found glpat-ABCDEFGHIJKLMNOPQRST in the log");

        assert!(
            !out.text.contains("glpat-ABCDEFGHIJKLMNOPQRST"),
            "{}",
            out.text
        );
        assert!(out.text.contains("[REDACTED:gitlab-pat]"), "{}", out.text);
        assert!(!out.replacements[0].known);
    }

    /// A provisioned value must be reported by path even when it
    /// also matches a pattern — the more informative answer wins.
    #[test]
    fn a_known_value_is_named_rather_than_pattern_redacted() {
        let pattern = gitlab_pattern();
        let s = Scrubber::new([("team/gitlab/token", "glpat-ABCDEFGHIJKLMNOPQRST")])
            .with_patterns([&pattern as &dyn SecretPattern]);

        let out = s.scrub("token glpat-ABCDEFGHIJKLMNOPQRST here");

        assert!(
            out.text.contains("@secret:team/gitlab/token"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("[REDACTED"), "{}", out.text);
    }

    #[test]
    fn text_without_secrets_passes_through_untouched() {
        let s = Scrubber::new([("p", "SECRETVALUE123")]);
        let out = s.scrub("nothing sensitive here");

        assert_eq!(out.text, "nothing sensitive here");
        assert!(!out.had_leak());
    }

    /// A very short "secret" would match everywhere and shred the
    /// log, which is worse than leaving it alone.
    #[test]
    fn implausibly_short_values_are_not_matched() {
        let s = Scrubber::new([("p", "ab")]);
        assert_eq!(s.known_value_count(), 0);

        let out = s.scrub("a table of absolutes");
        assert_eq!(out.text, "a table of absolutes");
    }

    /// Overlapping values must be replaced whole rather than in
    /// fragments that leave part of a secret behind.
    #[test]
    fn the_longest_overlapping_value_wins() {
        let s = Scrubber::new([
            ("short", "SECRETVALUE123"),
            ("long", "SECRETVALUE123EXTENDED"),
        ]);

        let out = s.scrub("here is SECRETVALUE123EXTENDED");

        assert!(!out.text.contains("SECRETVALUE123"), "{}", out.text);
        assert!(out.text.contains("@secret:long"), "{}", out.text);
    }

    #[test]
    fn an_empty_scrubber_is_a_no_op() {
        let s = Scrubber::new(Vec::<(String, String)>::new());
        let out = s.scrub("anything at all");

        assert_eq!(out.text, "anything at all");
        assert!(!out.had_leak());
    }

    /// The Debug impl is a leak surface of its own.
    #[test]
    fn debug_never_renders_the_values() {
        let s = Scrubber::new([("p", "SUPERSECRETVALUE")]);
        let rendered = format!("{s:?}");

        assert!(!rendered.contains("SUPERSECRETVALUE"), "{rendered}");
        assert!(rendered.contains("known_values"), "{rendered}");
    }

    /// The regression this pass was silently missing.
    ///
    /// Every earlier test here used a hand-written unanchored pattern,
    /// so the module looked correct while the real catalogue — which
    /// is anchored — could never match anything but a bare token.
    /// Using the shipped patterns is the whole point of the test.
    #[test]
    fn a_real_catalogue_pattern_matches_a_token_inside_a_sentence() {
        let s = Scrubber::new(Vec::<(String, String)>::new()).with_patterns(crate::builtins());

        let out = s.scrub("upstream returned 401: token glpat-ABCDEFGHIJKLMNOPQRSTU is invalid");

        assert!(
            !out.text.contains("glpat-ABCDEFGHIJKLMNOPQRSTU"),
            "the value survived: {}",
            out.text
        );
        assert!(out.text.contains("[REDACTED:gitlab-pat]"), "{}", out.text);
    }

    /// Most of the catalogue can scan; the ones that cannot are
    /// refused on purpose and must still be loaded for whole-string
    /// matching rather than dropped.
    #[test]
    fn the_catalogue_reports_how_much_of_it_can_scan() {
        let s = Scrubber::new(Vec::<(String, String)>::new()).with_patterns(crate::builtins());

        assert_eq!(s.pattern_count(), 31);
        assert_eq!(s.scanning_pattern_count(), 26);
    }

    /// The alias written by pass one must survive pass two. If a
    /// catalogue pattern matched `@secret:<path>` the log would say
    /// `[REDACTED:…]` and lose the one detail that makes it useful —
    /// which secret leaked.
    #[test]
    fn an_alias_from_the_first_pass_is_not_redacted_by_the_second() {
        let s = Scrubber::new([("team/gitlab/deploy-token", "glpat-ABCDEFGHIJKLMNOPQRSTU")])
            .with_patterns(crate::builtins());

        let out = s.scrub("push failed with glpat-ABCDEFGHIJKLMNOPQRSTU");

        assert!(
            out.text.contains("@secret:team/gitlab/deploy-token"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("[REDACTED"), "{}", out.text);
    }

    /// Ordinary output must come through untouched. A scrubber that
    /// eats commit hashes and file paths gets switched off, and then
    /// it protects nothing at all.
    #[test]
    fn ordinary_tool_output_passes_through_the_catalogue_untouched() {
        let s = Scrubber::new(Vec::<(String, String)>::new()).with_patterns(crate::builtins());

        let text = "commit 08a2981b047b0f8ffa464e80d5486e04ecaee460 touched \
                    crates/devboy-storage/src/source.rs (see https://github.com/meteora-pro/devboy-tools)";
        let out = s.scrub(text);

        assert_eq!(out.text, text, "replacements: {:?}", out.replacements);
        assert!(!out.had_leak());
    }

    /// Multi-byte input must not be sliced mid-character.
    #[test]
    fn unicode_text_is_handled_without_panicking() {
        let s = Scrubber::new([("p", "SECRETVALUE123")]);
        let out = s.scrub("ключ SECRETVALUE123 в тексте — и ещё эмодзи 🔐");

        assert!(!out.text.contains("SECRETVALUE123"));
        assert!(out.text.contains("🔐"));
        assert!(out.text.contains("ключ"));
    }
}
