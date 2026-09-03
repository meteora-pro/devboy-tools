//! Daemon-side TOTP state: the resident secret, the replay guard
//! and the rate limit (ADR-024 §1, Ф6c).
//!
//! # Where the guarantee actually comes from
//!
//! A TOTP code proves a human is present only because the agent
//! cannot mint one. That rests on a single fact: the shared secret
//! lives in daemon memory and nowhere the agent can read.
//!
//! Which is why [`TOTP_SECRET_PATH`] is unreachable through
//! `secret.get` and absent from `secret.list`. Without that, an
//! agent talking to an unlocked daemon would simply *ask* for the
//! secret, generate its own codes, and the whole re-unlock story
//! would be decoration. The reserved slot is not tidiness — it is
//! the mechanism.
//!
//! # Why a replay guard
//!
//! RFC 6238 §5.2: a code stays valid for its whole time step, and
//! the verifier must not accept the same step twice. Otherwise an
//! agent that observes one code — over a shoulder, in a screenshot,
//! in a log — can re-use it for the remainder of that window.
//!
//! # Why a rate limit
//!
//! Six digits is a million possibilities, but a step lasts thirty
//! seconds and an unthrottled attacker gets as many guesses as it
//! can make requests. The limit turns "guess the code" from a
//! throughput problem back into a probability one.

use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use devboy_vault_crypto::totp;

/// Reserved vault path holding the shared TOTP secret.
///
/// Follows the `__sources/` convention from ADR-021 §5: a leading
/// double underscore marks a namespace the framework owns and no
/// ordinary caller may read.
pub const TOTP_SECRET_PATH: &str = "__totp/secret";

/// Prefix of every framework-reserved path.
///
/// Checked as a prefix rather than an exact match so a future
/// reserved slot is closed by default rather than open until
/// someone remembers to add it.
pub const RESERVED_PREFIX: &str = "__totp/";

/// How many failed attempts are tolerated inside [`ATTEMPT_WINDOW`].
const MAX_ATTEMPTS: usize = 5;

/// Sliding window the attempts are counted over.
const ATTEMPT_WINDOW: Duration = Duration::from_secs(30);

/// How long the TOTP path stays shut after too many attempts.
const LOCKOUT: Duration = Duration::from_secs(60);

/// Why a TOTP re-unlock was refused.
///
/// These map onto the agent-facing error kinds from ADR-024 §8, and
/// they are deliberately distinct: "no secret resident" and "wrong
/// code" need different actions from the caller, and collapsing
/// them would send an agent into a retry loop it cannot win.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotpDenial {
    /// No secret is resident — the daemon has not been unlocked
    /// with a passphrase this boot, or none is enrolled.
    Unavailable,
    /// The code did not verify.
    BadCode,
    /// The code was valid but its time step was already used.
    Replayed,
    /// Too many attempts; the path is shut until the lockout ends.
    RateLimited {
        /// Seconds until attempts are accepted again.
        retry_after_seconds: u64,
    },
}

/// The daemon's TOTP state for one process lifetime.
///
/// Deliberately not `Clone`: a second copy of the secret is a
/// second place it can leak from, and there is no reason to have
/// one.
#[derive(Default)]
pub struct TotpSession {
    /// The shared secret, resident only after a passphrase unlock.
    secret: Option<Zeroizing<Vec<u8>>>,
    /// Highest time step already accepted (RFC 6238 §5.2).
    last_accepted_step: Option<u64>,
    /// Timestamps of recent failures, oldest first.
    recent_failures: Vec<Instant>,
    /// When the current lockout ends, if any.
    locked_until: Option<Instant>,
}

impl std::fmt::Debug for TotpSession {
    /// Never renders the secret — only whether one is resident.
    ///
    /// A `Debug` that prints the secret would undo the reserved
    /// slot the moment anyone logged this struct.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TotpSession")
            .field("secret_resident", &self.secret.is_some())
            .field("last_accepted_step", &self.last_accepted_step)
            .field("recent_failures", &self.recent_failures.len())
            .finish()
    }
}

impl TotpSession {
    /// A session with no secret resident.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt the secret read out of a freshly-unlocked vault.
    pub fn set_secret(&mut self, secret: Vec<u8>) {
        self.secret = Some(Zeroizing::new(secret));
    }

    /// Whether a code could be verified at all right now.
    pub fn is_available(&self) -> bool {
        self.secret.is_some()
    }

    /// Forget the secret and every guard.
    ///
    /// Called when the vault locks: after a re-lock the daemon must
    /// be back to "no TOTP path" rather than holding a secret that
    /// outlives the unlock it came with.
    pub fn clear(&mut self) {
        self.secret = None;
        self.last_accepted_step = None;
        self.recent_failures.clear();
        self.locked_until = None;
    }

    /// Verify `code`, enforcing the replay guard and rate limit.
    ///
    /// `now` is monotonic (for the limiter) and `unix_seconds` is
    /// wall-clock (for the step). They are separate arguments
    /// because they answer different questions and a single clock
    /// would make one of them wrong.
    pub fn verify(
        &mut self,
        code: &str,
        now: Instant,
        unix_seconds: u64,
    ) -> Result<(), TotpDenial> {
        if let Some(until) = self.locked_until {
            if now < until {
                return Err(TotpDenial::RateLimited {
                    retry_after_seconds: (until - now).as_secs().max(1),
                });
            }
            // Lockout served: start the count over rather than
            // leaving the old failures to trip it again immediately.
            self.locked_until = None;
            self.recent_failures.clear();
        }

        let Some(secret) = self.secret.as_ref() else {
            // Not a failed attempt: there is nothing to guess
            // against, so counting it would let an agent lock out a
            // path that was never open.
            return Err(TotpDenial::Unavailable);
        };

        match totp::verify(secret, code, unix_seconds) {
            Ok(step) => {
                if self.last_accepted_step.is_some_and(|last| step.0 <= last) {
                    // A replay is a failure for limiting purposes —
                    // otherwise replaying one observed code is an
                    // unlimited free probe.
                    self.record_failure(now);
                    return Err(TotpDenial::Replayed);
                }
                self.last_accepted_step = Some(step.0);
                self.recent_failures.clear();
                Ok(())
            }
            Err(_) => {
                self.record_failure(now);
                if self.recent_failures.len() >= MAX_ATTEMPTS {
                    self.locked_until = Some(now + LOCKOUT);
                    return Err(TotpDenial::RateLimited {
                        retry_after_seconds: LOCKOUT.as_secs(),
                    });
                }
                Err(TotpDenial::BadCode)
            }
        }
    }

    /// Record a failure and drop the ones that have aged out.
    fn record_failure(&mut self, now: Instant) {
        self.recent_failures
            .retain(|at| now.duration_since(*at) < ATTEMPT_WINDOW);
        self.recent_failures.push(now);
    }
}

/// Whether `path` is a framework-reserved slot that no caller may
/// read, list, or write.
///
/// Used by the daemon on `secret.get`, `secret.list` and
/// `secret.put` alike. Excluding it from reads while allowing
/// writes would let an agent overwrite the secret with one of its
/// own — a subtler way to mint valid codes.
pub fn is_reserved(path: &str) -> bool {
    path.starts_with(RESERVED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A secret whose codes we can compute alongside the session.
    const SECRET: &[u8] = b"12345678901234567890";

    fn code_at(unix_seconds: u64) -> String {
        totp::code_for_step(SECRET, totp::step_at(unix_seconds)).expect("code")
    }

    fn session() -> TotpSession {
        let mut s = TotpSession::new();
        s.set_secret(SECRET.to_vec());
        s
    }

    #[test]
    fn a_valid_code_is_accepted() {
        let mut s = session();
        let now = Instant::now();
        assert_eq!(
            s.verify(&code_at(1_700_000_000), now, 1_700_000_000),
            Ok(())
        );
    }

    /// The RFC 6238 §5.2 rule, and the reason it exists: a code
    /// observed once must not work for the rest of its window.
    #[test]
    fn the_same_step_cannot_be_used_twice() {
        let mut s = session();
        let now = Instant::now();
        let t = 1_700_000_000;
        let code = code_at(t);

        assert_eq!(s.verify(&code, now, t), Ok(()));
        assert_eq!(
            s.verify(&code, now, t + 1),
            Err(TotpDenial::Replayed),
            "replaying a code inside its own step must be refused"
        );
    }

    /// ...including a step *older* than one already accepted, which
    /// is the same attack with a captured earlier code.
    ///
    /// Both codes are presented at the same wall-clock instant and
    /// both are inside the ±1-step skew window, so the only thing
    /// that can refuse the older one is the replay guard. Reaching
    /// further back would just produce an invalid code and prove
    /// nothing about replay.
    #[test]
    fn an_older_step_is_refused_after_a_newer_one() {
        let mut s = session();
        let now = Instant::now();
        let t = 1_700_000_000;
        let step = totp::step_at(t);

        let newer = totp::code_for_step(SECRET, step + 1).expect("code");
        let older = totp::code_for_step(SECRET, step).expect("code");

        assert_eq!(s.verify(&newer, now, t), Ok(()));
        assert_eq!(
            s.verify(&older, now, t),
            Err(TotpDenial::Replayed),
            "a captured earlier code must not work once a later one has been accepted"
        );
    }

    #[test]
    fn a_later_step_is_accepted_after_an_earlier_one() {
        let mut s = session();
        let now = Instant::now();
        let t = 1_700_000_000;

        assert_eq!(s.verify(&code_at(t), now, t), Ok(()));
        assert_eq!(s.verify(&code_at(t + 60), now, t + 60), Ok(()));
    }

    /// No secret resident is its own answer, not a wrong code — the
    /// caller's next move is completely different.
    #[test]
    fn no_resident_secret_reports_unavailable() {
        let mut s = TotpSession::new();
        assert!(!s.is_available());
        assert_eq!(
            s.verify("123456", Instant::now(), 1_700_000_000),
            Err(TotpDenial::Unavailable)
        );
    }

    /// A re-lock must take the TOTP path with it. A secret that
    /// outlived its unlock would let a code re-open a vault the user
    /// deliberately closed.
    #[test]
    fn clearing_the_session_removes_the_path() {
        let mut s = session();
        assert!(s.is_available());
        s.clear();
        assert!(!s.is_available());
        assert_eq!(
            s.verify("123456", Instant::now(), 1_700_000_000),
            Err(TotpDenial::Unavailable)
        );
    }

    #[test]
    fn wrong_codes_are_refused() {
        let mut s = session();
        assert_eq!(
            s.verify("000000", Instant::now(), 1_700_000_000),
            Err(TotpDenial::BadCode)
        );
    }

    /// Six digits is only a million guesses if guessing is cheap.
    #[test]
    fn too_many_wrong_codes_shut_the_path() {
        let mut s = session();
        let now = Instant::now();

        for attempt in 1..MAX_ATTEMPTS {
            assert_eq!(
                s.verify("000000", now, 1_700_000_000),
                Err(TotpDenial::BadCode),
                "attempt {attempt} should be an ordinary refusal"
            );
        }

        assert_eq!(
            s.verify("000000", now, 1_700_000_000),
            Err(TotpDenial::RateLimited {
                retry_after_seconds: LOCKOUT.as_secs()
            }),
            "the {MAX_ATTEMPTS}th failure should start the lockout"
        );
    }

    /// A correct code during the lockout is still refused —
    /// otherwise the limit is trivially bypassed by whoever is
    /// guessing.
    #[test]
    fn even_a_valid_code_is_refused_during_lockout() {
        let mut s = session();
        let now = Instant::now();
        let t = 1_700_000_000;

        for _ in 0..MAX_ATTEMPTS {
            let _ = s.verify("000000", now, t);
        }

        assert!(matches!(
            s.verify(&code_at(t), now, t),
            Err(TotpDenial::RateLimited { .. })
        ));
    }

    #[test]
    fn the_path_reopens_once_the_lockout_expires() {
        let mut s = session();
        let start = Instant::now();
        let t = 1_700_000_000;

        for _ in 0..MAX_ATTEMPTS {
            let _ = s.verify("000000", start, t);
        }

        let after = start + LOCKOUT + Duration::from_secs(1);
        assert_eq!(
            s.verify(&code_at(t), after, t),
            Ok(()),
            "the lockout must end, or one burst of noise locks the user out for good"
        );
    }

    /// Failures that aged out of the window must not count, or a
    /// user who mistypes once a day eventually gets locked out.
    #[test]
    fn failures_outside_the_window_do_not_accumulate() {
        let mut s = session();
        let start = Instant::now();
        let t = 1_700_000_000;

        for i in 0..20 {
            // One failure per window, spaced so none overlap.
            let at = start + ATTEMPT_WINDOW * (i + 1);
            assert_eq!(
                s.verify("000000", at, t),
                Err(TotpDenial::BadCode),
                "spaced-out mistakes must never trip the limiter"
            );
        }
    }

    /// An unavailable path must not be lockable: otherwise an agent
    /// could shut a door that was never open, and keep it shut.
    #[test]
    fn attempts_against_an_absent_secret_do_not_trip_the_limiter() {
        let mut s = TotpSession::new();
        let now = Instant::now();

        for _ in 0..(MAX_ATTEMPTS * 3) {
            assert_eq!(
                s.verify("000000", now, 1_700_000_000),
                Err(TotpDenial::Unavailable)
            );
        }

        // And once a secret arrives, the path works immediately.
        s.set_secret(SECRET.to_vec());
        assert_eq!(
            s.verify(&code_at(1_700_000_000), now, 1_700_000_000),
            Ok(())
        );
    }

    /// A successful verify clears the failure count, so a user who
    /// fumbles then succeeds is not one mistake from a lockout.
    #[test]
    fn a_success_resets_the_failure_count() {
        let mut s = session();
        let now = Instant::now();
        let t = 1_700_000_000;

        for _ in 0..(MAX_ATTEMPTS - 1) {
            let _ = s.verify("000000", now, t);
        }
        assert_eq!(s.verify(&code_at(t), now, t), Ok(()));

        for _ in 0..(MAX_ATTEMPTS - 1) {
            assert_eq!(
                s.verify("000000", now, t + 60),
                Err(TotpDenial::BadCode),
                "the count should have restarted after the success"
            );
        }
    }

    #[test]
    fn the_reserved_prefix_covers_the_secret_slot() {
        assert!(is_reserved(TOTP_SECRET_PATH));
        assert!(is_reserved("__totp/anything-future"));
        assert!(!is_reserved("team/github/token"));
        assert!(!is_reserved("personal/totp/token"));
    }

    /// Debug output is a common accidental leak path; this struct
    /// holds the one secret the whole scheme depends on.
    #[test]
    fn debug_output_never_contains_the_secret() {
        let s = session();
        let rendered = format!("{s:?}");

        assert!(rendered.contains("secret_resident"));
        assert!(
            !rendered.contains("12345678901234567890"),
            "the shared secret must never reach Debug output: {rendered}"
        );
    }
}
