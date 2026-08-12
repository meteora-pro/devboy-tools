//! Smoke tests for the daemon harness itself (ADR-024 test track
//! T1).
//!
//! A harness that silently fails to isolate, or that reports
//! success for a daemon that never started, would make every test
//! built on it worthless. These check the harness before anything
//! relies on it.

// The daemon is UNIX-only, and so is everything that
// drives it here: UNIX domain sockets, `SO_PEERCRED`,
// process reparenting. Off UNIX this compiles to an
// empty test binary rather than a build failure.
#![cfg(unix)]

mod common;

use common::{DaemonHarness, SpawnMode};

/// The harness must never touch the developer's real vault or
/// socket — every path lives inside the temp dir it owns.
#[test]
fn harness_isolates_all_paths_under_a_temp_dir() {
    let h = DaemonHarness::prepare();

    let socket = h.socket_path().to_path_buf();
    let vault = h.vault_path().to_path_buf();

    for path in [&socket, &vault] {
        let s = path.display().to_string();
        assert!(
            s.contains("tmp") || s.contains("Temp"),
            "harness path escaped the temp dir: {s}"
        );
        assert!(
            !s.contains("/.config/devboy-tools/"),
            "harness must not point at the real config dir: {s}"
        );
    }

    assert_ne!(socket, vault);
}

/// The socket path must stay well under the `sun_path` limit
/// (~108 bytes), or `bind` fails with a confusing error on
/// machines with long temp paths.
#[test]
fn socket_path_fits_in_sun_path() {
    let h = DaemonHarness::prepare();
    let len = h.socket_path().display().to_string().len();
    assert!(
        len < 100,
        "socket path is {len} bytes, too close to the limit"
    );
}

/// Fail-closed, end to end: a daemon spawned as a direct child of
/// this test is exactly the layout ADR-024 §7 check B refuses.
///
/// This is the harness's most important capability — without a
/// real process in a real parent relationship the check cannot be
/// tested at all.
#[test]
fn a_child_daemon_refuses_to_start_and_explains_why() {
    let mut h = DaemonHarness::prepare();

    let failure = h
        .start(SpawnMode::Child, false)
        .expect_err("a session-parented daemon must refuse to start");

    assert!(
        failure.mentions("refusing to start"),
        "expected a refusal, got:\n{}",
        failure.stderr
    );
    assert!(
        failure.mentions("DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON"),
        "the refusal must name the documented override:\n{}",
        failure.stderr
    );
    assert!(
        failure.mentions("systemctl --user start devboy-secrets")
            || failure.mentions("launchctl")
            || failure.mentions("Start-Service"),
        "the refusal must name the platform start command:\n{}",
        failure.stderr
    );
}

/// The override permits startup and changes nothing else: the
/// daemon still reports the real trust level and still withholds
/// TOTP.
#[test]
fn the_override_starts_the_daemon_but_does_not_launder_its_trust_level() {
    let mut h = DaemonHarness::prepare();

    h.start(SpawnMode::Child, true)
        .expect("override should permit startup");

    let out = h.take_output();
    assert!(
        out.stderr.contains("trust_level=agent_parented"),
        "override must not upgrade the reported trust level:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("totp_available=false"),
        "override must not resurrect the TOTP path:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("WARNING"),
        "override must warn on every launch:\n{}",
        out.stderr
    );
}
