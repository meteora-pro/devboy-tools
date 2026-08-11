//! Writing audit records, with scrubbing that cannot be skipped
//! (ADR-024 §4, Ф9c).
//!
//! # The problem this shape solves
//!
//! An audit record's free-text `detail` is the obvious place for a
//! secret to end up. Not maliciously — someone writes
//! `format!("failed: {e}")` and the error happens to quote the
//! value it was handed. A comment saying "remember to scrub" holds
//! until the first person who does not read it.
//!
//! So [`ScrubbedDetail`] has no public constructor. The only way to
//! obtain one is [`AuditWriter::scrub`], which runs the text through
//! the reverse-scrubber first. A caller cannot pass an unscrubbed
//! string to [`AuditWriter::record`] because the type does not exist
//! until the scrubber has produced it.
//!
//! # The gap this cannot close
//!
//! [`Scrubber`] skips values shorter than eight bytes — a
//! two-character "secret" would match everywhere and turn the log
//! into confetti. That is the right trade, but it means a short
//! secret passes through unscrubbed and *silently*.
//!
//! [`AuditWriter::unscrubbable_values`] reports how many values were
//! skipped, so a caller can say so rather than leaving the user to
//! assume the log is clean. A leak you cannot see is worse than one
//! you can.

use std::path::Path;

use devboy_secret_patterns::scrubber::Scrubber;
use devboy_vault_crypto::aead::KEY_LEN;
use devboy_vault_crypto::audit::{AuditError, AuditLog, AuditRecord};

/// Text that has been through the scrubber.
///
/// Deliberately opaque and constructible only via
/// [`AuditWriter::scrub`] — that is the whole mechanism. Making the
/// field public, or adding a `From<String>`, would quietly turn the
/// guarantee back into a convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubbedDetail(String);

impl ScrubbedDetail {
    /// The scrubbed text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Appends scrubbed records to an audit log.
pub struct AuditWriter {
    log: AuditLog,
    scrubber: Scrubber,
    /// How many values were too short for the scrubber to match.
    unscrubbable: usize,
}

impl AuditWriter {
    /// Open (or create) the log and build a scrubber over the
    /// values that must never appear in it.
    ///
    /// `values` is `(path, value)` for every secret currently
    /// known — typically everything the unlocked vault holds.
    pub fn open<I, P, V>(path: &Path, values: I) -> Result<Self, AuditError>
    where
        I: IntoIterator<Item = (P, V)>,
        P: Into<String>,
        V: AsRef<str>,
    {
        // Count before handing them over: the scrubber drops the
        // short ones and cannot tell us afterwards how many it
        // dropped.
        let collected: Vec<(String, String)> = values
            .into_iter()
            .map(|(p, v)| (p.into(), v.as_ref().to_owned()))
            .collect();
        let offered = collected.len();

        let scrubber = Scrubber::new(collected.iter().map(|(p, v)| (p.clone(), v.clone())));
        let unscrubbable = offered.saturating_sub(scrubber.known_value_count());

        Ok(Self {
            log: AuditLog::open_or_create(path)?,
            scrubber,
            unscrubbable,
        })
    }

    /// How many known values are too short for the scrubber to
    /// match, and will therefore pass through untouched.
    ///
    /// Non-zero means the log is not fully protected and the user
    /// should be told — silently is exactly how this kind of leak
    /// survives.
    pub fn unscrubbable_values(&self) -> usize {
        self.unscrubbable
    }

    /// A warning to surface when some values cannot be scrubbed.
    pub fn scrub_warning(&self) -> Option<String> {
        (self.unscrubbable > 0).then(|| {
            format!(
                "{} secret value(s) are shorter than the scrubber's minimum match length, so \
                 they would not be redacted if they reached the audit log. Consider rotating \
                 them to a longer value.",
                self.unscrubbable
            )
        })
    }

    /// Run text through the scrubber, producing the only type
    /// [`Self::record`] will accept.
    pub fn scrub(&self, text: &str) -> ScrubbedDetail {
        ScrubbedDetail(self.scrubber.scrub(text).text)
    }

    /// Append one record.
    ///
    /// `detail` can only have come from [`Self::scrub`], which is
    /// what makes the redaction structural rather than a habit.
    pub fn record(
        &mut self,
        key: &[u8; KEY_LEN],
        action: &str,
        path: &str,
        actor: &str,
        detail: Option<ScrubbedDetail>,
    ) -> Result<u64, AuditError> {
        let record = AuditRecord {
            timestamp: now_iso8601(),
            action: action.to_owned(),
            path: path.to_owned(),
            actor: actor.to_owned(),
            detail: detail.map(|d| d.0),
        };
        self.log.append(key, &record)
    }

    /// Records written so far.
    pub fn count(&self) -> u64 {
        self.log.count()
    }

    /// Read the log back.
    pub fn read_all(&self, key: &[u8; KEY_LEN]) -> Result<Vec<AuditRecord>, AuditError> {
        self.log.read_all(key)
    }
}

/// Current time as an ISO 8601 string.
///
/// Second precision: an audit trail wants to answer "when, roughly"
/// and finer resolution only sharpens the timing side-channel the
/// encryption is there to blunt.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Civil-from-days, so the crate needs no date dependency.
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's `civil_from_days`, for 1970-01-01 epoch days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = [3u8; KEY_LEN];

    fn writer(dir: &tempfile::TempDir, values: Vec<(&str, &str)>) -> AuditWriter {
        AuditWriter::open(&dir.path().join("audit-log.dvb"), values).expect("open")
    }

    /// The point of the whole module: a value that reaches the
    /// detail text is replaced by its path.
    #[test]
    fn a_known_value_is_redacted_before_it_reaches_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = writer(&dir, vec![("team/github/token", "ghp-supersecretvalue")]);

        let detail = w.scrub("request failed with token ghp-supersecretvalue");
        w.record(&KEY, "read", "team/github/token", "agent", Some(detail))
            .expect("record");

        let records = w.read_all(&KEY).expect("read");
        let text = records[0].detail.as_deref().unwrap();
        assert!(
            !text.contains("ghp-supersecretvalue"),
            "the value survived into the log: {text}"
        );
        assert!(
            text.contains("team/github/token"),
            "the redaction should name the path: {text}"
        );
    }

    /// The log is encrypted at rest, so no detail text is legible
    /// in the file.
    ///
    /// Note what this does *not* prove. A mutation run that
    /// bypassed the scrubber left this test green — encryption
    /// alone hides the value, whether it was redacted or not. The
    /// scrubbing guarantee is carried by
    /// `a_known_value_is_redacted_before_it_reaches_the_log`, which
    /// inspects the decrypted record. This one guards a different
    /// regression: someone making the log plaintext.
    #[test]
    fn the_log_is_encrypted_at_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut w = AuditWriter::open(&path, vec![("team/github/token", "ghp-supersecretvalue")])
            .expect("open");

        let detail = w.scrub("saw ghp-supersecretvalue in the response");
        w.record(&KEY, "read", "team/github/token", "agent", Some(detail))
            .expect("record");

        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows("ghp-supersecretvalue".len())
                .any(|win| win == b"ghp-supersecretvalue"),
            "the raw value appears in the audit file"
        );
        assert!(
            !bytes
                .windows("in the response".len())
                .any(|win| win == b"in the response"),
            "the detail text is legible in the file, so the log is not encrypted"
        );
    }

    /// The type is the mechanism: `record` takes `ScrubbedDetail`,
    /// which only `scrub` produces. This test documents that intent
    /// — the real enforcement is the compiler refusing anything
    /// else.
    #[test]
    fn detail_can_only_be_built_by_scrubbing() {
        let dir = tempfile::tempdir().unwrap();
        let w = writer(&dir, vec![("a/b/c", "0123456789abcdef")]);

        let scrubbed = w.scrub("nothing sensitive here");
        assert_eq!(scrubbed.as_str(), "nothing sensitive here");

        // There is no `ScrubbedDetail::new`, no public field and no
        // `From<String>`: constructing one without the scrubber does
        // not compile.
    }

    /// A short value cannot be scrubbed, and the user has to be
    /// told. Silence here is a leak nobody can see.
    #[test]
    fn values_too_short_to_scrub_are_counted_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let w = writer(
            &dir,
            vec![
                ("a/long/one", "0123456789abcdef"),
                ("a/short/one", "abc"),
                ("another/short", "xy"),
            ],
        );

        assert_eq!(
            w.unscrubbable_values(),
            2,
            "both short values should be counted as unprotected"
        );
        let warning = w.scrub_warning().expect("a warning is due");
        assert!(warning.contains('2'), "{warning}");
        assert!(
            warning.contains("rotating"),
            "the warning should say what to do: {warning}"
        );
    }

    #[test]
    fn no_warning_when_every_value_can_be_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let w = writer(&dir, vec![("a/b/c", "0123456789abcdef")]);

        assert_eq!(w.unscrubbable_values(), 0);
        assert!(w.scrub_warning().is_none());
    }

    #[test]
    fn records_carry_the_action_path_and_actor() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = writer(&dir, vec![]);
        w.record(&KEY, "unlock", "vault", "user", None)
            .expect("record");

        let records = w.read_all(&KEY).expect("read");
        assert_eq!(records[0].action, "unlock");
        assert_eq!(records[0].path, "vault");
        assert_eq!(records[0].actor, "user");
        assert!(records[0].detail.is_none());
    }

    #[test]
    fn the_timestamp_is_a_plausible_iso8601_instant() {
        let stamp = now_iso8601();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert!(
            stamp.starts_with("20"),
            "a timestamp outside this century suggests broken date maths: {stamp}"
        );
        // Separators in the right places.
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[7..8], "-");
        assert_eq!(&stamp[10..11], "T");
    }

    /// A known epoch second, so the hand-rolled date maths is
    /// pinned rather than merely plausible.
    #[test]
    fn the_date_conversion_matches_a_known_instant() {
        // 2026-08-11 is 20676 days after the epoch.
        assert_eq!(civil_from_days(20_676), (2026, 8, 11));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // A leap day, where off-by-one maths usually shows up.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
