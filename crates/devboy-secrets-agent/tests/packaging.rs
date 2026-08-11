//! The shipped service definitions must actually encode the
//! ADR-024 §7 properties they exist for (Ф5a).
//!
//! These files are the answer to "start it independently
//! instead" — the instruction the daemon prints when it refuses.
//! If a unit lost `StandardInput=null`, or started relaunching a
//! refused daemon in a loop, the refusal message would be
//! pointing at something that reintroduces the very condition it
//! is complaining about. Nothing else in the build would notice.

use std::path::PathBuf;

fn packaging_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/devboy-secrets-agent.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("packaging")
}

fn systemd_unit() -> String {
    std::fs::read_to_string(packaging_dir().join("systemd/devboy-secrets.service"))
        .expect("systemd unit is shipped")
}

fn launchd_plist() -> String {
    std::fs::read_to_string(packaging_dir().join("launchd/dev.devboy.secrets.plist"))
        .expect("launchd plist is shipped")
}

/// A user unit, not a system one: a system service would have to
/// multiplex every user's keys through one process or run as
/// root, both of which ADR-024 §7 rules out.
#[test]
fn systemd_unit_installs_into_the_user_manager() {
    let unit = systemd_unit();
    assert!(unit.contains("WantedBy=default.target"), "{unit}");
    assert!(
        unit.contains("%h/"),
        "a user unit should resolve paths through %h rather than hardcoding a home"
    );
    assert!(
        !unit.contains("User=") && !unit.contains("Group="),
        "a user unit must not try to set User=/Group="
    );
}

/// Check C: the daemon must not inherit a controlling terminal.
/// `StandardInput=null` is what actually guarantees it.
#[test]
fn systemd_unit_denies_a_controlling_terminal() {
    assert!(
        systemd_unit().contains("StandardInput=null"),
        "without this the unit can inherit a TTY and trip check C"
    );
}

/// Exit code 1 is how the daemon reports a failed provenance
/// check. Relaunching that forever would bury the explanation in
/// the journal and turn a clear refusal into a restart loop.
#[test]
fn systemd_unit_does_not_loop_on_a_refusal() {
    let unit = systemd_unit();
    assert!(
        unit.contains("RestartPreventExitStatus=1"),
        "a refused daemon must not be restarted in a loop:\n{unit}"
    );
    assert!(unit.contains("Restart=on-failure"), "{unit}");
}

/// The vault key lives in this process's memory and is zeroized
/// on drop. A core dump would defeat that entirely.
#[test]
fn systemd_unit_keeps_the_key_out_of_core_dumps() {
    let unit = systemd_unit();
    assert!(unit.contains("LimitCORE=0"), "{unit}");
    assert!(unit.contains("MemoryDenyWriteExecute=yes"), "{unit}");
    assert!(unit.contains("UMask=0077"), "{unit}");
}

/// ADR-023 §3.3 gives the SIGTERM zeroize path 10 seconds; the
/// stop timeout has to leave room for it.
#[test]
fn systemd_unit_allows_time_for_the_zeroize_path() {
    let unit = systemd_unit();
    assert!(unit.contains("KillSignal=SIGTERM"), "{unit}");

    let timeout: u64 = unit
        .lines()
        .find_map(|l| l.trim().strip_prefix("TimeoutStopSec="))
        .expect("unit sets a stop timeout")
        .trim()
        .parse()
        .expect("stop timeout is a number");

    assert!(
        timeout >= 10,
        "ADR-023 §3.3 allows the cleanup 10s; TimeoutStopSec={timeout} would cut it short"
    );
}

/// The plist must parse as XML and carry the keys the daemon
/// depends on.
#[test]
fn launchd_plist_is_well_formed_and_complete() {
    let plist = launchd_plist();

    assert!(plist.starts_with("<?xml"), "plist must be XML");
    assert_eq!(
        plist.matches("<dict>").count(),
        plist.matches("</dict>").count(),
        "unbalanced <dict> elements"
    );
    assert_eq!(
        plist.matches("<array>").count(),
        plist.matches("</array>").count(),
        "unbalanced <array> elements"
    );

    for key in [
        "dev.devboy.secrets",
        "ProgramArguments",
        "RunAtLoad",
        "KeepAlive",
    ] {
        assert!(plist.contains(key), "plist is missing `{key}`");
    }
}

/// Same two §7 properties as the systemd unit: no controlling
/// terminal, and no relaunch loop on a refusal.
#[test]
fn launchd_plist_encodes_the_same_guarantees() {
    let plist = launchd_plist();

    assert!(
        plist.contains("<key>StandardInPath</key>") && plist.contains("/dev/null"),
        "the agent must not inherit a terminal:\n{plist}"
    );
    assert!(
        plist.contains("<key>SuccessfulExit</key>"),
        "KeepAlive must be conditional so a refusal is not relaunched forever"
    );
    assert!(
        plist.contains("<key>Core</key>"),
        "core dumps must be disabled to protect the zeroized key"
    );
}

/// launchd does not expand `~` or consult `PATH`, so a relative
/// program path silently never starts.
#[test]
fn launchd_program_path_is_absolute() {
    let plist = launchd_plist();
    let program = plist
        .split("<key>ProgramArguments</key>")
        .nth(1)
        .and_then(|s| s.split("<string>").nth(1))
        .and_then(|s| s.split("</string>").next())
        .expect("plist names a program");

    assert!(
        program.starts_with('/'),
        "launchd needs an absolute path, got `{program}`"
    );
    assert!(program.ends_with("devboy-secrets-agent"), "got `{program}`");
}

/// Both definitions must point at the same binary name the
/// refusal message tells users to start.
#[test]
fn both_units_reference_the_daemon_binary() {
    assert!(systemd_unit().contains("devboy-secrets-agent"));
    assert!(launchd_plist().contains("devboy-secrets-agent"));
}
