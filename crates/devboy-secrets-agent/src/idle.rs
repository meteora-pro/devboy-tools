//! Idle-timeout + SIGTERM handling for the agent daemon per
//! [ADR-023] §3.3 lifecycle.
//!
//! # Idle timeout
//!
//! ADR-023 §3.3 mandates that the unlocked vault key is zeroized
//! 15 minutes after the last user activity. "Activity" is anything
//! that proves the user is still around: a `secret.get`, a
//! `metadata.update`, a `secret.put`, or a `secret.list`.
//! `vault.status` and `vault.lock` are *not* counted as activity
//! (they are no-ops on the unlocked-state cache).
//!
//! Implementation:
//!
//! 1. The server records the wall-clock time of the last activity in
//!    `last_activity` (an [`Instant`]).
//! 2. Before every dispatched request, the server calls
//!    [`IdleClock::now`] and checks whether `now - last_activity >
//!    idle_timeout`. If yes, the cached `Vault` is dropped (which
//!    zeroizes the [`SecretBox`](secrecy::SecretBox) holding the
//!    32-byte vault key) before the request handler runs. The next
//!    `secret.*` operation therefore sees `vault: None` and returns
//!    `VAULT_LOCKED`.
//!
//! No background timer task is needed — the check runs on every
//! request, and a connection that has been silent for the timeout
//! window will get a `VAULT_LOCKED` response on its next call.
//! The only thing this design misses is auto-locking *between*
//! requests, but that is acceptable for v1: the secret is locked
//! before any value can be served, which is the property that
//! matters.
//!
//! # SIGTERM handling
//!
//! ADR-023 §3.3: "the daemon traps SIGTERM, zeroizes, flushes
//! pending writes, and exits within 10 seconds." The
//! [`install_sigterm_handler`] helper sets up a tokio signal stream
//! that, on SIGTERM, drops the supplied `VaultServer` (zeroizing the
//! key) and runs a caller-provided cleanup callback before exiting
//! the process. The 10-second cap is enforced by an
//! [`tokio::time::timeout`] around the cleanup.
//!
//! # Time abstraction
//!
//! The [`IdleClock`] trait lets tests substitute a manual clock so
//! the 15-minute timeout can be exercised without sleeping. The
//! production [`SystemClock`] returns [`Instant::now`].
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default idle window — ADR-023 §3.3 specifies 15 minutes.
///
/// ADR-024 §2 generalises this into [`UnlockWindow`]: the fixed
/// idle re-lock becomes one of three configurable bounds, and the
/// 15-minute value survives as the `strict` profile's default.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// How long an unlock lasts, and what can end it early (ADR-024 §2).
///
/// Three bounds, because "how long is the vault open" has three
/// different answers that a single number conflated:
///
/// - `unlock_ttl` — how long *this* unlock lasts by default;
/// - `max_unlock_ttl` — the ceiling no single unlock may exceed,
///   including one that explicitly asks for longer;
/// - `idle_relock` — an optional early close on inactivity, off
///   under `convenient` so the daily-unlock intent survives.
///
/// The window is a maximum, not a promise: explicit `vault.lock`,
/// the SIGTERM zeroize, and process exit all still end it sooner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnlockWindow {
    /// Default duration of an unlock.
    pub unlock_ttl: Duration,
    /// Hard ceiling on any single unlock.
    pub max_unlock_ttl: Duration,
    /// Re-lock after this much inactivity, if set.
    pub idle_relock: Option<Duration>,
}

impl UnlockWindow {
    /// The `convenient` profile: unlock once for the working day.
    pub fn convenient() -> Self {
        Self {
            unlock_ttl: Duration::from_secs(8 * 60 * 60),
            max_unlock_ttl: Duration::from_secs(24 * 60 * 60),
            idle_relock: None,
        }
    }

    /// The `strict` profile: short window plus idle re-lock.
    ///
    /// Pairs with forcing `approve_on_use` to per-call at the
    /// policy layer — that, not the smaller numbers, is what
    /// mitigates an agent waiting out a legitimate unlock.
    pub fn strict() -> Self {
        Self {
            unlock_ttl: DEFAULT_IDLE_TIMEOUT,
            max_unlock_ttl: Duration::from_secs(60 * 60),
            idle_relock: Some(Duration::from_secs(5 * 60)),
        }
    }

    /// Build from configured seconds, clamping `unlock_ttl` to the
    /// ceiling.
    ///
    /// Clamping rather than erroring keeps a misconfigured daemon
    /// usable; the config layer reports the inconsistency
    /// separately so it is visible rather than silently absorbed.
    pub fn from_seconds(unlock_ttl: u64, max_unlock_ttl: u64, idle_relock: Option<u64>) -> Self {
        Self {
            unlock_ttl: Duration::from_secs(unlock_ttl.min(max_unlock_ttl)),
            max_unlock_ttl: Duration::from_secs(max_unlock_ttl),
            idle_relock: idle_relock.map(Duration::from_secs),
        }
    }

    /// Build the window the user actually configured.
    ///
    /// The profile supplies the defaults and the explicit
    /// `secrets.*` keys override them — which is what
    /// [`Config::unlock_ttl_seconds`](devboy_core::config::Config::unlock_ttl_seconds)
    /// and its siblings already resolve.
    ///
    /// Until this existed the daemon ran on
    /// [`UnlockWindow::default`] no matter what the user had set,
    /// so choosing the `strict` profile changed nothing about how
    /// long an unlock lasted.
    pub fn from_config(config: &devboy_core::config::Config) -> Self {
        Self::from_seconds(
            config.unlock_ttl_seconds(),
            config.max_unlock_ttl_seconds(),
            config.idle_relock_seconds(),
        )
    }

    /// Resolve how long a specific unlock should last.
    ///
    /// A caller may request a longer window ("I am leaving a task
    /// running overnight"), but never past `max_unlock_ttl` — the
    /// ceiling is the user's standing decision and a per-call
    /// argument does not override it.
    pub fn resolve(&self, requested: Option<Duration>) -> Duration {
        requested
            .unwrap_or(self.unlock_ttl)
            .min(self.max_unlock_ttl)
    }
}

impl Default for UnlockWindow {
    fn default() -> Self {
        Self::convenient()
    }
}

/// Maximum time the SIGTERM cleanup handler may take per ADR-023 §3.3
/// ("…and exits within 10 seconds").
pub const SIGTERM_GRACE: Duration = Duration::from_secs(10);

// =============================================================================
// IdleClock — testable time source
// =============================================================================

/// Wall-clock abstraction so tests can race past the 15-minute
/// timeout without `tokio::time::sleep`.
pub trait IdleClock: Send + Sync {
    /// Current monotonic time. Production uses [`Instant::now`];
    /// [`ManualClock`] returns whatever the test arranged.
    fn now(&self) -> Instant;
}

/// Production clock backed by [`Instant::now`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl IdleClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test clock whose `now()` value is controlled by [`ManualClock::advance`].
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<Mutex<Instant>>);

impl ManualClock {
    /// Build a manual clock starting at `initial`.
    pub fn new(initial: Instant) -> Self {
        Self(Arc::new(Mutex::new(initial)))
    }

    /// Advance the clock by `delta`. Subsequent calls to
    /// [`IdleClock::now`] return the new time.
    pub fn advance(&self, delta: Duration) {
        let mut guard = self.0.lock().expect("ManualClock mutex poisoned");
        *guard += delta;
    }
}

impl IdleClock for ManualClock {
    fn now(&self) -> Instant {
        *self.0.lock().expect("ManualClock mutex poisoned")
    }
}

// =============================================================================
// Idle tracker
// =============================================================================

/// Idle-timeout state carried by the [`crate::server::VaultServer`].
///
/// Built when the vault is unlocked (`record_unlock`), cleared when
/// the vault is locked (`record_lock`), and queried before every
/// request via [`IdleTracker::should_auto_lock`].
pub struct IdleTracker {
    /// Configured idle window.
    ///
    /// Retained as the idle bound; the overall unlock lifetime now
    /// lives in [`Self::window`].
    pub idle_timeout: Duration,
    /// Time of the last activity, or `None` when the vault is locked.
    pub last_activity: Option<Instant>,
    /// Time source. Tests inject a [`ManualClock`].
    pub clock: Arc<dyn IdleClock>,
    /// The configured unlock window (ADR-024 §2).
    pub window: UnlockWindow,
    /// When the current unlock expires, or `None` when locked.
    ///
    /// Distinct from `last_activity`: activity extends the *idle*
    /// bound but never the overall window, so a busy session still
    /// re-locks on schedule.
    pub expires_at: Option<Instant>,
}

impl IdleTracker {
    /// Build a tracker with the default 15-minute timeout and
    /// [`SystemClock`].
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_IDLE_TIMEOUT)
    }

    /// Build a tracker with a caller-supplied timeout and
    /// [`SystemClock`]. Useful for `~/.devboy/config.toml`-driven
    /// configuration.
    pub fn with_timeout(idle_timeout: Duration) -> Self {
        Self {
            idle_timeout,
            last_activity: None,
            clock: Arc::new(SystemClock),
            window: UnlockWindow {
                idle_relock: Some(idle_timeout),
                ..UnlockWindow::default()
            },
            expires_at: None,
        }
    }

    /// Build a tracker with a caller-supplied clock. Used by tests
    /// that want to fast-forward without sleeping.
    pub fn with_clock(idle_timeout: Duration, clock: Arc<dyn IdleClock>) -> Self {
        Self {
            idle_timeout,
            last_activity: None,
            clock,
            window: UnlockWindow {
                idle_relock: Some(idle_timeout),
                ..UnlockWindow::default()
            },
            expires_at: None,
        }
    }

    /// Build a tracker for a configured unlock window (ADR-024 §2).
    pub fn with_window(window: UnlockWindow, clock: Arc<dyn IdleClock>) -> Self {
        Self {
            idle_timeout: window.idle_relock.unwrap_or(window.unlock_ttl),
            last_activity: None,
            clock,
            window,
            expires_at: None,
        }
    }

    /// Reset the activity timestamp to now. Called after a successful
    /// `vault.unlock`.
    pub fn record_unlock(&mut self) {
        self.record_unlock_for(None);
    }

    /// Record an unlock that requested a specific duration.
    ///
    /// The request is honoured up to `max_unlock_ttl`; a caller
    /// asking for longer gets the ceiling rather than an error,
    /// because the ceiling is the user's standing decision and
    /// failing the unlock would just strand them.
    pub fn record_unlock_for(&mut self, requested: Option<Duration>) {
        let now = self.clock.now();
        self.last_activity = Some(now);
        self.expires_at = Some(now + self.window.resolve(requested));
    }

    /// Clear the activity timestamp. Called after `vault.lock` or
    /// after the auto-lock fires.
    pub fn record_lock(&mut self) {
        self.last_activity = None;
        self.expires_at = None;
    }

    /// When the current unlock expires, if the vault is unlocked.
    pub fn expires_at(&self) -> Option<Instant> {
        self.expires_at
    }

    /// Seconds left in the current unlock window, for
    /// `secrets_status()`.
    pub fn remaining_seconds(&self) -> Option<u64> {
        let expires = self.expires_at?;
        Some(
            expires
                .saturating_duration_since(self.clock.now())
                .as_secs(),
        )
    }

    /// Bump the activity timestamp on a successful "real" operation
    /// (`secret.get`, `secret.list`, `secret.put`, `secret.rotate`,
    /// `metadata.update`). Has no effect when the vault is locked.
    pub fn record_activity(&mut self) {
        if self.last_activity.is_some() {
            self.last_activity = Some(self.clock.now());
        }
    }

    /// `true` iff the vault is unlocked and either bound has been
    /// crossed (ADR-024 §2). The caller's job after `true` is to
    /// drop the unlocked vault.
    ///
    /// Two independent reasons, and both must be checked:
    ///
    /// - the unlock **window** expired — this fires even in a busy
    ///   session, which is the point of having a ceiling at all;
    /// - **idle re-lock** is configured and nothing has happened
    ///   for that long.
    ///
    /// Under `convenient` the second is off, so a working day is
    /// one unlock. Under `strict` both apply.
    pub fn should_auto_lock(&self) -> bool {
        let Some(last) = self.last_activity else {
            return false;
        };
        let now = self.clock.now();

        if let Some(expires) = self.expires_at
            && now >= expires
        {
            return true;
        }

        match self.window.idle_relock {
            Some(idle) => now.saturating_duration_since(last) > idle,
            // Pre-ADR-024 callers built the tracker through
            // `with_timeout`, which sets `idle_relock`; a window
            // with no idle bound relies solely on `expires_at`.
            None => false,
        }
    }
}

impl Default for IdleTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for IdleTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdleTracker")
            .field("idle_timeout", &self.idle_timeout)
            .field("last_activity", &self.last_activity)
            .field("clock", &"<dyn IdleClock>")
            .finish()
    }
}

// =============================================================================
// SIGTERM handling
// =============================================================================

/// Install a SIGTERM handler that, on signal:
///
/// 1. Calls `cleanup` (typically: drop the `VaultServer` to zeroize
///    the vault key, flush pending writes).
/// 2. Caps the cleanup duration at [`SIGTERM_GRACE`] (10 seconds per
///    ADR-023 §3.3).
/// 3. Returns from the awaited future so the caller can exit the
///    process.
///
/// The function awaits indefinitely on `tokio::signal` until SIGTERM
/// arrives. Spawn it as a tokio task and `await` to drive shutdown
/// from `main`.
///
/// On non-Unix platforms this is a no-op that never returns (since
/// SIGTERM doesn't exist there).
#[cfg(unix)]
pub async fn install_sigterm_handler<F, Fut>(cleanup: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut sig = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to install SIGTERM handler; daemon will not zeroize on signal");
            return;
        }
    };
    sig.recv().await;
    tracing::info!(
        timeout = ?SIGTERM_GRACE,
        "SIGTERM received, running cleanup with grace timeout"
    );
    if tokio::time::timeout(SIGTERM_GRACE, cleanup())
        .await
        .is_err()
    {
        tracing::warn!(
            timeout = ?SIGTERM_GRACE,
            "SIGTERM cleanup did not finish in time; exiting anyway"
        );
    }
}

/// Non-Unix stub. Future named-pipe transport will need its own
/// shutdown signal; for now this is a placeholder that never
/// returns.
#[cfg(not(unix))]
pub async fn install_sigterm_handler<F, Fut>(_cleanup: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // No SIGTERM on Windows; future named-pipe transport will get
    // its own shutdown story.
    std::future::pending::<()>().await;
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod unlock_window_tests {
    use super::*;

    fn tracker(window: UnlockWindow) -> (IdleTracker, ManualClock) {
        let clock = ManualClock::new(Instant::now());
        let tracker = IdleTracker::with_window(window, Arc::new(clock.clone()));
        (tracker, clock)
    }

    #[test]
    fn profiles_carry_the_adr_024_windows() {
        let c = UnlockWindow::convenient();
        assert_eq!(c.unlock_ttl, Duration::from_secs(8 * 60 * 60));
        assert_eq!(c.max_unlock_ttl, Duration::from_secs(24 * 60 * 60));
        assert_eq!(c.idle_relock, None, "convenient must preserve daily unlock");

        let s = UnlockWindow::strict();
        assert_eq!(s.unlock_ttl, Duration::from_secs(15 * 60));
        assert_eq!(s.max_unlock_ttl, Duration::from_secs(60 * 60));
        assert_eq!(s.idle_relock, Some(Duration::from_secs(5 * 60)));
    }

    /// The ceiling is the user's standing decision; a per-call
    /// argument cannot raise it.
    #[test]
    fn a_requested_duration_cannot_exceed_the_ceiling() {
        let w = UnlockWindow::strict();
        assert_eq!(w.resolve(None), w.unlock_ttl);
        assert_eq!(
            w.resolve(Some(Duration::from_secs(120))),
            Duration::from_secs(120)
        );
        assert_eq!(
            w.resolve(Some(Duration::from_secs(86_400))),
            w.max_unlock_ttl,
            "an overlong request is clamped, not rejected"
        );
    }

    #[test]
    fn configured_seconds_clamp_the_default_too() {
        let w = UnlockWindow::from_seconds(86_400, 3_600, Some(60));
        assert_eq!(w.unlock_ttl, Duration::from_secs(3_600));
        assert_eq!(w.idle_relock, Some(Duration::from_secs(60)));
    }

    /// The central property of §2: the window is a ceiling on the
    /// unlock, not on inactivity. A session that stays busy the
    /// whole time still re-locks on schedule.
    #[test]
    fn the_window_expires_even_under_continuous_activity() {
        let (mut t, clock) = tracker(UnlockWindow {
            unlock_ttl: Duration::from_secs(100),
            max_unlock_ttl: Duration::from_secs(1_000),
            idle_relock: None,
        });
        t.record_unlock();

        for _ in 0..9 {
            clock.advance(Duration::from_secs(10));
            t.record_activity();
            assert!(!t.should_auto_lock(), "still inside the window");
        }

        clock.advance(Duration::from_secs(10));
        t.record_activity();
        assert!(
            t.should_auto_lock(),
            "activity must not extend the overall window"
        );
    }

    /// Under `convenient` there is no idle bound, so a long quiet
    /// stretch inside the window does not re-lock.
    #[test]
    fn convenient_does_not_relock_on_idleness() {
        let (mut t, clock) = tracker(UnlockWindow::convenient());
        t.record_unlock();

        clock.advance(Duration::from_secs(4 * 60 * 60));
        assert!(!t.should_auto_lock(), "4h idle is fine inside an 8h window");
    }

    /// Under `strict` both bounds apply, and idleness is the one
    /// that usually fires first.
    #[test]
    fn strict_relocks_on_idleness_before_the_window_ends() {
        let (mut t, clock) = tracker(UnlockWindow::strict());
        t.record_unlock();

        clock.advance(Duration::from_secs(4 * 60));
        assert!(!t.should_auto_lock(), "under the 5m idle bound");

        clock.advance(Duration::from_secs(2 * 60));
        assert!(
            t.should_auto_lock(),
            "6m idle exceeds the strict idle bound"
        );
    }

    #[test]
    fn a_longer_requested_window_is_honoured_within_the_ceiling() {
        let (mut t, clock) = tracker(UnlockWindow::convenient());
        t.record_unlock_for(Some(Duration::from_secs(20 * 60 * 60)));

        clock.advance(Duration::from_secs(12 * 60 * 60));
        assert!(
            !t.should_auto_lock(),
            "12h is inside the 20h window that was asked for"
        );

        clock.advance(Duration::from_secs(9 * 60 * 60));
        assert!(t.should_auto_lock(), "21h is past it");
    }

    #[test]
    fn locking_clears_both_bounds() {
        let (mut t, _clock) = tracker(UnlockWindow::strict());
        t.record_unlock();
        assert!(t.expires_at().is_some());

        t.record_lock();
        assert!(t.expires_at().is_none());
        assert!(
            !t.should_auto_lock(),
            "a locked vault never auto-locks again"
        );
    }

    #[test]
    fn remaining_seconds_counts_down_and_floors_at_zero() {
        let (mut t, clock) = tracker(UnlockWindow {
            unlock_ttl: Duration::from_secs(100),
            max_unlock_ttl: Duration::from_secs(100),
            idle_relock: None,
        });
        assert_eq!(t.remaining_seconds(), None, "locked vault has no remainder");

        t.record_unlock();
        assert_eq!(t.remaining_seconds(), Some(100));

        clock.advance(Duration::from_secs(40));
        assert_eq!(t.remaining_seconds(), Some(60));

        clock.advance(Duration::from_secs(500));
        assert_eq!(t.remaining_seconds(), Some(0), "must saturate, not panic");
    }

    /// Pre-ADR-024 construction paths keep their old semantics.
    #[test]
    fn with_timeout_still_behaves_as_an_idle_timeout() {
        let clock = ManualClock::new(Instant::now());
        let mut t = IdleTracker::with_clock(Duration::from_secs(60), Arc::new(clock.clone()));
        t.record_unlock();

        clock.advance(Duration::from_secs(30));
        t.record_activity();
        clock.advance(Duration::from_secs(30));
        assert!(!t.should_auto_lock(), "activity resets the idle bound");

        clock.advance(Duration::from_secs(31));
        assert!(t.should_auto_lock());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_start() -> Instant {
        Instant::now()
    }

    // -- ManualClock -------------------------------------------------------

    #[test]
    fn manual_clock_returns_initial_time() {
        let t0 = fixed_start();
        let clock = ManualClock::new(t0);
        assert_eq!(clock.now(), t0);
    }

    #[test]
    fn manual_clock_advance_moves_time_forward() {
        let t0 = fixed_start();
        let clock = ManualClock::new(t0);
        clock.advance(Duration::from_secs(60));
        let after = clock.now();
        assert_eq!(after - t0, Duration::from_secs(60));
    }

    #[test]
    fn manual_clock_advance_is_additive() {
        let t0 = fixed_start();
        let clock = ManualClock::new(t0);
        clock.advance(Duration::from_secs(5));
        clock.advance(Duration::from_secs(3));
        assert_eq!(clock.now() - t0, Duration::from_secs(8));
    }

    // -- IdleTracker -------------------------------------------------------

    fn fast_tracker() -> (IdleTracker, ManualClock) {
        let t0 = fixed_start();
        let clock = ManualClock::new(t0);
        let tracker = IdleTracker::with_clock(
            Duration::from_secs(10), // 10-second timeout for tests
            Arc::new(clock.clone()),
        );
        (tracker, clock)
    }

    #[test]
    fn locked_tracker_does_not_auto_lock() {
        let (tracker, _clock) = fast_tracker();
        // Default state: locked, no last_activity.
        assert!(!tracker.should_auto_lock());
    }

    #[test]
    fn unlock_then_no_advance_does_not_auto_lock() {
        let (mut tracker, _clock) = fast_tracker();
        tracker.record_unlock();
        assert!(!tracker.should_auto_lock());
    }

    #[test]
    fn unlock_then_advance_within_timeout_does_not_auto_lock() {
        let (mut tracker, clock) = fast_tracker();
        tracker.record_unlock();
        clock.advance(Duration::from_secs(5)); // < 10s timeout
        assert!(!tracker.should_auto_lock());
    }

    #[test]
    fn unlock_then_advance_past_timeout_auto_locks() {
        let (mut tracker, clock) = fast_tracker();
        tracker.record_unlock();
        clock.advance(Duration::from_secs(11)); // > 10s timeout
        assert!(tracker.should_auto_lock());
    }

    #[test]
    fn record_activity_resets_the_idle_clock() {
        let (mut tracker, clock) = fast_tracker();
        tracker.record_unlock();
        clock.advance(Duration::from_secs(8));
        // Activity bump just before the timeout.
        tracker.record_activity();
        clock.advance(Duration::from_secs(8));
        // Total elapsed since unlock = 16s, but only 8s since the
        // activity bump → still under the 10s timeout.
        assert!(!tracker.should_auto_lock());
    }

    #[test]
    fn record_activity_when_locked_is_a_noop() {
        // Calling record_activity without an unlock first must not
        // accidentally start the timer — the tracker should remain
        // in the locked state.
        let (mut tracker, clock) = fast_tracker();
        tracker.record_activity();
        clock.advance(Duration::from_secs(100));
        assert!(!tracker.should_auto_lock());
        assert!(tracker.last_activity.is_none());
    }

    #[test]
    fn record_lock_clears_last_activity() {
        let (mut tracker, _clock) = fast_tracker();
        tracker.record_unlock();
        assert!(tracker.last_activity.is_some());
        tracker.record_lock();
        assert!(tracker.last_activity.is_none());
    }

    #[test]
    fn auto_lock_threshold_is_strict_inequality() {
        // `should_auto_lock` uses `>` (not `>=`), so exactly-equal
        // elapsed time should NOT auto-lock yet.
        let (mut tracker, clock) = fast_tracker();
        tracker.record_unlock();
        clock.advance(Duration::from_secs(10)); // == timeout, not >
        assert!(!tracker.should_auto_lock());
        clock.advance(Duration::from_micros(1));
        assert!(tracker.should_auto_lock());
    }

    // -- Constructor variants ----------------------------------------------

    #[test]
    fn default_constructor_uses_15_minute_timeout() {
        let tracker = IdleTracker::new();
        assert_eq!(tracker.idle_timeout, DEFAULT_IDLE_TIMEOUT);
        assert_eq!(tracker.idle_timeout, Duration::from_secs(900));
    }

    #[test]
    fn with_timeout_overrides_default() {
        let tracker = IdleTracker::with_timeout(Duration::from_secs(60));
        assert_eq!(tracker.idle_timeout, Duration::from_secs(60));
    }

    #[test]
    fn debug_redacts_clock() {
        // The clock field is a `dyn` trait object; Debug output
        // should not try to print it (and the manual clock's Debug
        // would expose nothing meaningful anyway). Verify the
        // hand-rolled impl prints a placeholder.
        let tracker = IdleTracker::new();
        let dbg = format!("{tracker:?}");
        assert!(dbg.contains("IdleTracker"));
        assert!(dbg.contains("<dyn IdleClock>"));
    }

    // -- Constants ---------------------------------------------------------

    #[test]
    fn default_timeout_matches_adr_023() {
        assert_eq!(DEFAULT_IDLE_TIMEOUT, Duration::from_secs(15 * 60));
    }

    #[test]
    fn sigterm_grace_matches_adr_023() {
        assert_eq!(SIGTERM_GRACE, Duration::from_secs(10));
    }

    /// Choosing `strict` has to change the window the daemon runs.
    ///
    /// It did not, for the whole epic: `UnlockWindow::from_config`
    /// did not exist and the server always built from
    /// `UnlockWindow::default()`, so this assertion is the wire that
    /// was missing rather than a restatement of the profile table.
    #[test]
    fn the_strict_profile_reaches_the_window_the_daemon_enforces() {
        use devboy_core::config::Config;

        let mut config = Config::default();
        config.set("secrets.profile", "strict").unwrap();

        let window = UnlockWindow::from_config(&config);

        assert_eq!(
            window.unlock_ttl,
            UnlockWindow::strict().unlock_ttl,
            "the strict profile's window must survive the trip through config"
        );
        assert_ne!(
            window.unlock_ttl,
            UnlockWindow::convenient().unlock_ttl,
            "if strict and convenient produce the same window, the profile is doing nothing"
        );
        assert!(
            window.idle_relock.is_some(),
            "strict promises idle re-lock; a window without it is not strict"
        );
    }

    #[test]
    fn the_default_profile_yields_the_convenient_window() {
        use devboy_core::config::Config;

        let window = UnlockWindow::from_config(&Config::default());
        assert_eq!(window.unlock_ttl, UnlockWindow::convenient().unlock_ttl);
        assert_eq!(
            window.max_unlock_ttl,
            UnlockWindow::convenient().max_unlock_ttl
        );
    }

    /// An explicit key beats the profile's default, and the ceiling
    /// still wins over both — a user cannot widen their own limit by
    /// setting a larger TTL.
    #[test]
    fn explicit_settings_override_the_profile_but_not_the_ceiling() {
        use devboy_core::config::Config;

        let mut config = Config::default();
        config.set("secrets.max_unlock_ttl_seconds", "600").unwrap();
        config.set("secrets.unlock_ttl_seconds", "99999").unwrap();

        let window = UnlockWindow::from_config(&config);
        assert_eq!(window.max_unlock_ttl, Duration::from_secs(600));
        assert_eq!(
            window.unlock_ttl,
            Duration::from_secs(600),
            "a TTL above the ceiling must clamp, not raise the ceiling"
        );
    }
}
