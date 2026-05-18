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
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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
    pub idle_timeout: Duration,
    /// Time of the last activity, or `None` when the vault is locked.
    pub last_activity: Option<Instant>,
    /// Time source. Tests inject a [`ManualClock`].
    pub clock: Arc<dyn IdleClock>,
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
        }
    }

    /// Build a tracker with a caller-supplied clock. Used by tests
    /// that want to fast-forward without sleeping.
    pub fn with_clock(idle_timeout: Duration, clock: Arc<dyn IdleClock>) -> Self {
        Self {
            idle_timeout,
            last_activity: None,
            clock,
        }
    }

    /// Reset the activity timestamp to now. Called after a successful
    /// `vault.unlock`.
    pub fn record_unlock(&mut self) {
        self.last_activity = Some(self.clock.now());
    }

    /// Clear the activity timestamp. Called after `vault.lock` or
    /// after the auto-lock fires.
    pub fn record_lock(&mut self) {
        self.last_activity = None;
    }

    /// Bump the activity timestamp on a successful "real" operation
    /// (`secret.get`, `secret.list`, `secret.put`, `secret.rotate`,
    /// `metadata.update`). Has no effect when the vault is locked.
    pub fn record_activity(&mut self) {
        if self.last_activity.is_some() {
            self.last_activity = Some(self.clock.now());
        }
    }

    /// `true` iff the vault is currently unlocked **and** the last
    /// activity was longer than [`Self::idle_timeout`] ago. The
    /// caller's job after `true` is to drop the unlocked vault.
    pub fn should_auto_lock(&self) -> bool {
        match self.last_activity {
            Some(last) => self.clock.now().saturating_duration_since(last) > self.idle_timeout,
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
}
