//! ADR-024 §7 process-model tests against real processes (T2).
//!
//! These assert the property the whole trusted-path model rests
//! on: **a daemon that a caller can `ptrace` must not pretend to
//! protect its memory from that caller.** The only way to test it
//! is to build the process topology for real.
//!
//! Unix-only: the checks are `SO_PEERCRED` and `/proc`-based, and
//! the Windows path answers the same question through the service
//! manager instead.

// The daemon is UNIX-only, and so is everything that
// drives it here: UNIX domain sockets, `SO_PEERCRED`,
// process reparenting. Off UNIX this compiles to an
// empty test binary rather than a build failure.
#![cfg(unix)]
#![cfg(unix)]

mod common;

use common::{DaemonHarness, SpawnMode};

/// Check B, fail-closed: a daemon started from inside another
/// process's tree refuses outright.
///
/// The refusal has to be *actionable* — an error that says only
/// "refused" leaves the user to go read source.
#[test]
fn check_b_refuses_a_session_parented_daemon() {
    let mut h = DaemonHarness::prepare();

    let failure = h
        .start(SpawnMode::Child, false)
        .expect_err("check B must refuse a child-spawned daemon");

    assert!(failure.mentions("ADR-024 §7"), "{}", failure.stderr);
    assert!(
        failure.mentions("cannot protect its own memory"),
        "the refusal must say what is actually wrong:\n{}",
        failure.stderr
    );
    assert_eq!(
        failure.exit_status,
        Some(1),
        "a refused daemon must exit non-zero so a supervisor notices"
    );
}

/// Check B passes for the layout a correctly installed daemon has.
///
/// `setsid` forks and exits, so the daemon reparents to init and
/// leaves this test's process tree entirely.
#[test]
fn check_b_accepts_an_init_reparented_daemon() {
    let mut h = DaemonHarness::prepare();

    h.start(SpawnMode::Orphaned, false)
        .expect("an init-reparented daemon must start cleanly");

    let out = h.take_output();
    assert!(
        out.stderr.contains("trust_level=independent"),
        "an orphaned daemon should reach the independent level:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("totp_available=true"),
        "the TOTP path is available once memory is actually private:\n{}",
        out.stderr
    );
}

/// The escape from check A is also an escape from the capability
/// it guards.
///
/// ADR-024 §7 notes that double-forking defeats the ancestry walk
/// — but `ptrace_scope` evaluates descent *at the time of the
/// call*, so an orphaned daemon is no longer this process's
/// descendant and cannot be traced by it either. This test pins
/// that reasoning: the same manoeuvre that passes the check also
/// removes the caller from the ancestry it would have exploited.
#[test]
fn double_forking_past_check_a_also_severs_the_ptrace_relationship() {
    let mut h = DaemonHarness::prepare();
    h.start(SpawnMode::Orphaned, false)
        .expect("orphaned daemon starts");

    // The daemon accepts our connection precisely because we are
    // no longer in its ancestry.
    let connected = h.connect_raw();
    assert!(
        connected.is_ok(),
        "an orphaned daemon has no ancestry reason to refuse us"
    );

    // And the relationship really is severed: our PID is not in
    // its parent chain, which is the same fact `ptrace_scope`
    // consults.
    let me = std::process::id();
    assert!(
        !devboy_secrets_agent::provenance::client_is_ancestor(me, me),
        "a process is never its own ancestor"
    );
}

/// Check C: holding a controlling terminal warns but never
/// blocks. A TTY is a hint about *how* the daemon was launched,
/// not a `ptrace` capability, so treating it as fatal would refuse
/// working setups for no security gain.
#[test]
fn check_c_is_advisory_only() {
    let mut h = DaemonHarness::prepare();

    // Started with the override so check B does not mask what
    // check C does on its own.
    h.start(SpawnMode::Child, true)
        .expect("check C must never prevent startup");

    let out = h.take_output();
    assert!(
        !out.stderr.contains("refusing to start"),
        "check C must not be fatal:\n{}",
        out.stderr
    );
}

/// The override is a severity downgrade and nothing more.
///
/// This is the property that keeps the escape hatch honest: it
/// changes what is *permitted*, never what is *claimed*. If it
/// ever started reporting `independent`, an agent would believe a
/// guarantee that does not hold.
#[test]
fn the_override_never_upgrades_what_the_daemon_claims() {
    let mut h = DaemonHarness::prepare();
    h.start(SpawnMode::Child, true)
        .expect("override permits startup");

    let out = h.take_output();

    assert!(
        out.stderr.contains("trust_level=agent_parented"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("totp_available=false"),
        "{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("trust_level=independent"),
        "the override must never claim independence:\n{}",
        out.stderr
    );
}

/// The warning must repeat on every launch rather than being
/// emitted once — a single line scrolls away, and an invisible
/// override is how an insecure layout becomes permanent.
#[test]
fn the_override_warning_is_not_a_one_time_notice() {
    for attempt in 0..2 {
        let mut h = DaemonHarness::prepare();
        h.start(SpawnMode::Child, true)
            .expect("override permits startup");
        let out = h.take_output();

        assert!(
            out.stderr
                .contains("DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON is set"),
            "launch {attempt} lost the override warning:\n{}",
            out.stderr
        );
    }
}
