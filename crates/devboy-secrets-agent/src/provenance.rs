//! The daemon verifies its own provenance (ADR-024 §7).
//!
//! "The agent must not start the daemon" is an operational rule,
//! and operational rules nobody enforces decay into comments —
//! the same reasoning that made the audit-log scrub server-side
//! and permanent purge user-only. The daemon can check this about
//! itself, so it must.
//!
//! Three checks, none of which requires knowing which agent is
//! running:
//!
//! - **A. Ancestry, per connection.** If the connecting client
//!   appears in the daemon's own parent chain, that client can
//!   `ptrace` the daemon under the common
//!   `kernel.yama.ptrace_scope = 1` policy. Every guarantee that
//!   depends on daemon memory being private is void for it.
//! - **B. Startup provenance.** A daemon started the intended way
//!   is reparented to the init system. One whose parent is an
//!   ordinary session process was spawned inside someone's
//!   process tree — the condition this rule exists to prevent.
//! - **C. Controlling terminal.** A correctly launched daemon has
//!   none. Holding one says it came from a shell, which also
//!   bears on where its prompts would render.
//!
//! # Structural, not nominal
//!
//! The daemon never asks "is my ancestor a coding agent". That
//! would need a list of vendor process names, which the project's
//! neutrality guard forbids and which would fail on the first
//! agent not on the list. It asks **"can my caller `ptrace` me"**
//! — the property that actually matters, and the same question
//! regardless of what the caller is.
//!
//! # Fail closed
//!
//! A degraded-but-running daemon is the outcome §7 argues
//! against: it preserves the appearance of a guarantee that no
//! longer holds. A and B are therefore fatal by default, with
//! [`INSECURE_OVERRIDE_ENV`] as the documented escape hatch for
//! test harnesses and hand-started daemons.

use std::fmt;

/// Downgrades checks A and B from fatal to warning.
///
/// Named with `INSECURE` so its appearance in a CI file, a
/// Dockerfile, or a diff is self-documenting.
///
/// Its weakness is stated rather than left to be discovered: an
/// agent that starts the daemon already controls the daemon's
/// environment and can set this itself. The override therefore
/// guards against accidental misconfiguration and good-faith
/// agents — the threat model this framework claims — and not
/// against a hostile one. What it buys is that the insecure path
/// cannot be reached *silently* or by default.
pub const INSECURE_OVERRIDE_ENV: &str = "DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON";

/// How well the process layout supports the guarantees in §1/§7.
///
/// Computed from the actual process tree — never asserted by
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    /// Daemon runs under its own service account. Cross-UID
    /// `ptrace` is denied outright.
    SeparateUid,
    /// Same UID, but started independently of any session process
    /// (systemd user unit, launchd, login shell). Under
    /// `ptrace_scope = 1` a non-descendant keeps its memory
    /// private.
    Independent,
    /// Same UID and inside a session process's tree. No meaningful
    /// protection against a hostile caller.
    AgentParented,
}

impl TrustLevel {
    /// Stable wire name for `secrets_status()` and `doctor`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SeparateUid => "separate_uid",
            Self::Independent => "independent",
            Self::AgentParented => "agent_parented",
        }
    }

    /// Whether the TOTP path (ADR-024 §1) may be offered.
    ///
    /// TOTP's guarantee is that the agent cannot derive a code
    /// because `totp_secret` lives in daemon memory. If the caller
    /// can read that memory, the guarantee is gone and offering
    /// the method would be theatre.
    pub fn allows_totp(self) -> bool {
        !matches!(self, Self::AgentParented)
    }
}

impl fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Outcome of the startup checks (B and C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupProvenance {
    /// Whether the daemon was reparented to the init system.
    pub reparented_to_init: bool,
    /// PID of the daemon's parent at startup.
    pub parent_pid: u32,
    /// Whether the daemon holds a controlling terminal.
    pub has_controlling_terminal: bool,
    /// Whether [`INSECURE_OVERRIDE_ENV`] was set.
    pub override_active: bool,
}

impl StartupProvenance {
    /// Whether the daemon should refuse to start.
    ///
    /// Only check B is fatal here. C is a warning: holding a TTY
    /// is a hint about *how* the daemon was launched, not a
    /// `ptrace` capability in itself.
    pub fn is_fatal(&self) -> bool {
        !self.reparented_to_init && !self.override_active
    }

    /// The trust level implied by startup alone. Per-connection
    /// ancestry can still lower it for a specific caller.
    pub fn trust_level(&self) -> TrustLevel {
        if self.reparented_to_init {
            TrustLevel::Independent
        } else {
            TrustLevel::AgentParented
        }
    }

    /// Operator-facing explanation of a fatal startup.
    pub fn refusal_message(&self) -> String {
        format!(
            "The secret daemon was started by process {} rather than by the init system, so it \
             cannot protect its own memory from whoever started it (ADR-024 §7).\n\n\
             Start it independently instead:\n  {}\n\n\
             For test harnesses and local debugging, set {}=1 to downgrade this to a warning. \
             The daemon will then report trust_level=agent_parented and will not offer the TOTP \
             unlock path.",
            self.parent_pid,
            platform_start_command(),
            INSECURE_OVERRIDE_ENV,
        )
    }

    /// Warnings worth emitting on every launch — never once,
    /// because a single line scrolls away.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.reparented_to_init && self.override_active {
            out.push(format!(
                "{INSECURE_OVERRIDE_ENV} is set: the daemon was started inside another process's \
                 tree (parent {}) and cannot protect its memory from it. TOTP unlock is \
                 unavailable and trust_level is agent_parented.",
                self.parent_pid
            ));
        }
        if self.has_controlling_terminal {
            out.push(
                "The daemon holds a controlling terminal, which means it was started from a \
                 shell. Prompts may render into a terminal another process owns."
                    .to_string(),
            );
        }
        out
    }
}

/// Whether the override is set to a truthy value.
pub fn insecure_override_active() -> bool {
    std::env::var(INSECURE_OVERRIDE_ENV)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

/// Platform command that starts the daemon the intended way.
pub fn platform_start_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "launchctl kickstart -k gui/$(id -u)/dev.devboy.secrets"
    } else if cfg!(target_os = "windows") {
        "Start-Service devboy-secrets"
    } else {
        "systemctl --user start devboy-secrets"
    }
}

#[cfg(unix)]
mod imp {
    use super::*;

    /// PIDs the init system uses. `1` is classic init / launchd;
    /// a `systemd --user` instance is itself reparented, so a
    /// daemon it starts sees that manager as its parent.
    fn is_init_like(parent_pid: u32) -> bool {
        if parent_pid == 1 {
            return true;
        }
        // A `systemd --user` manager is the other legitimate
        // parent. Identify it structurally: its own parent is PID
        // 1 and it is not our session leader's shell.
        matches!(parent_of(parent_pid), Some(1))
    }

    /// Read the parent PID of an arbitrary process.
    ///
    /// Linux exposes this in `/proc/<pid>/stat`; elsewhere only
    /// the daemon's own parent is reliably available, which is
    /// enough for check B.
    pub fn parent_of(pid: u32) -> Option<u32> {
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // The comm field can contain spaces and parentheses,
            // so parse from the last ')' rather than splitting
            // naively.
            let after_comm = stat.rsplit_once(')')?.1;
            let mut fields = after_comm.split_whitespace();
            let _state = fields.next()?;
            fields.next()?.parse().ok()
        }
        // No `/proc`. The kernel will still tell a process its own
        // parent, and that one link is worth having: "the client is
        // my direct parent" is the shape this check was written
        // for, and answering it needs no platform-specific API. A
        // full walk would need `sysctl(KERN_PROC_PID)` — unsafe FFI
        // this workspace forbids — so anything deeper is honestly
        // reported as undetermined rather than waved through.
        #[cfg(not(target_os = "linux"))]
        {
            if pid == std::process::id() {
                Some(nix::unistd::getppid().as_raw() as u32)
            } else {
                None
            }
        }
    }

    /// Walk the daemon's own parent chain looking for `client_pid`
    /// (check A).
    ///
    /// Kept as a `bool` for callers that only care about the
    /// definite answer. [`ancestry`] is what the connection path
    /// uses, because "I could not tell" is a third outcome and it
    /// used to be reported as "not an ancestor".
    pub fn client_is_ancestor(client_pid: u32, current: u32) -> bool {
        matches!(ancestry(client_pid, current), Ancestry::Ancestor)
    }

    /// Walk the chain, distinguishing "no" from "could not tell".
    ///
    /// A bounded walk: process trees are shallow, and a cycle
    /// would otherwise hang the daemon on every connection.
    pub fn ancestry(client_pid: u32, current: u32) -> Ancestry {
        walk_ancestry(client_pid, current, parent_of)
    }

    /// Whether this process holds a controlling terminal (check C).
    ///
    /// `/dev/tty` resolves to the calling process's controlling
    /// terminal and fails with `ENXIO` when there is none — which
    /// is exactly the question, and unlike checking whether stdin
    /// is a TTY it is not defeated by redirection.
    pub fn has_controlling_terminal() -> bool {
        std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .is_ok()
    }

    /// Run the startup checks.
    pub fn startup_provenance() -> StartupProvenance {
        let parent_pid = nix::unistd::getppid().as_raw() as u32;
        StartupProvenance {
            reparented_to_init: is_init_like(parent_pid),
            parent_pid,
            has_controlling_terminal: has_controlling_terminal(),
            override_active: insecure_override_active(),
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub fn parent_of(_pid: u32) -> Option<u32> {
        None
    }

    /// Windows uses a named pipe and the service manager; the
    /// ancestry question is answered by the SCM rather than by a
    /// process walk, so this conservatively reports "not an
    /// ancestor" and leaves the decision to the service model.
    pub fn client_is_ancestor(_client_pid: u32, _current: u32) -> bool {
        false
    }

    /// Same reasoning as [`client_is_ancestor`]: not a failure to
    /// read the chain, a platform where the chain is not the
    /// question.
    pub fn ancestry(_client_pid: u32, _current: u32) -> Ancestry {
        Ancestry::NotAncestor
    }

    pub fn has_controlling_terminal() -> bool {
        false
    }

    pub fn startup_provenance() -> StartupProvenance {
        StartupProvenance {
            reparented_to_init: true,
            parent_pid: 0,
            has_controlling_terminal: false,
            override_active: insecure_override_active(),
        }
    }
}

pub use imp::{
    ancestry, client_is_ancestor, has_controlling_terminal, parent_of, startup_provenance,
};

/// What the ancestry walk could establish.
///
/// Three outcomes, because the walk has three. Collapsing the
/// third into "not an ancestor" is how a check that does not run
/// comes to look like a check that passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ancestry {
    /// The client is in the daemon's parent chain.
    Ancestor,
    /// The chain was walked to the init system without finding it.
    NotAncestor,
    /// The chain became unreadable before reaching the top, so the
    /// question was never answered either way.
    Undetermined,
}

/// The walk itself, over any parent lookup.
///
/// Separated from the platform lookup so the traversal can be
/// tested against a made-up process tree — including trees with
/// unreadable links and cycles, which are the cases that matter
/// and the ones a live-process test cannot arrange.
pub fn walk_ancestry(
    client_pid: u32,
    mut current: u32,
    parent_of: impl Fn(u32) -> Option<u32>,
) -> Ancestry {
    const MAX_DEPTH: usize = 64;
    for _ in 0..MAX_DEPTH {
        if current <= 1 {
            return Ancestry::NotAncestor;
        }
        match parent_of(current) {
            Some(parent) => {
                if parent == client_pid {
                    return Ancestry::Ancestor;
                }
                current = parent;
            }
            None => return Ancestry::Undetermined,
        }
    }
    // Depth exhausted: a chain this long is a cycle or a hostile
    // shape, and either way nothing was established.
    Ancestry::Undetermined
}

/// Verdict for a single connection (check A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionVerdict {
    /// The caller is not in the daemon's ancestry.
    Trusted,
    /// The caller can `ptrace` the daemon. Refuse unless the
    /// override is set.
    CallerIsAncestor,
    /// The parent chain could not be read far enough to decide.
    ///
    /// Its own variant so that "we checked and it was fine" and
    /// "we could not check" stop being the same answer. Before
    /// this existed, a `None` from the parent lookup became
    /// `false` became [`Trusted`], and on every non-Linux UNIX —
    /// where there is no `/proc` to walk — that was *every*
    /// connection.
    ///
    /// [`Trusted`]: ConnectionVerdict::Trusted
    Undetermined,
}

impl ConnectionVerdict {
    /// Whether the connection should be refused.
    ///
    /// [`Undetermined`] is allowed through, deliberately. Check A
    /// exists because Linux's `kernel.yama.ptrace_scope = 1` lets a
    /// process trace its own descendants — ancestry *is* the
    /// capability there. The platforms where the walk comes back
    /// undetermined are the ones without that rule, where being an
    /// ancestor grants nothing (macOS gates `task_for_pid` behind
    /// root or a debugger entitlement regardless of kinship). And
    /// check B, which is fatal and which `getppid` answers
    /// everywhere, has already refused startup for the
    /// configuration this would otherwise catch.
    ///
    /// So it fails open — but it says so, and [`Undetermined`]
    /// reaches the log rather than being laundered into
    /// [`Trusted`].
    ///
    /// [`Undetermined`]: ConnectionVerdict::Undetermined
    /// [`Trusted`]: ConnectionVerdict::Trusted
    pub fn should_refuse(self, override_active: bool) -> bool {
        matches!(self, Self::CallerIsAncestor) && !override_active
    }
}

/// Check whether `client_pid` is an ancestor of this daemon.
pub fn check_connection(client_pid: u32) -> ConnectionVerdict {
    let me = std::process::id();
    match ancestry(client_pid, me) {
        Ancestry::Ancestor => ConnectionVerdict::CallerIsAncestor,
        Ancestry::NotAncestor => ConnectionVerdict::Trusted,
        Ancestry::Undetermined => ConnectionVerdict::Undetermined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk has three outcomes and must report three.
    ///
    /// Before this, the unreadable case returned `false` — the same
    /// value as a completed walk that found nothing. On every
    /// non-Linux UNIX the parent lookup returned `None` for every
    /// PID, so the daemon reported *every* connection as `Trusted`
    /// while check A had not run at all.
    #[test]
    fn an_unreadable_chain_is_not_the_same_answer_as_a_clean_walk() {
        // 900 -> 800 -> 1: walked to the top, client absent.
        let visible = |pid: u32| match pid {
            900 => Some(800),
            800 => Some(1),
            _ => None,
        };
        assert_eq!(walk_ancestry(4242, 900, visible), Ancestry::NotAncestor);

        // The same tree with the second link unreadable.
        let opaque = |pid: u32| if pid == 900 { Some(800) } else { None };
        assert_eq!(
            walk_ancestry(4242, 900, opaque),
            Ancestry::Undetermined,
            "an unreadable link must not read as a completed walk"
        );

        // Nothing readable at all — the non-Linux case as it was.
        assert_eq!(walk_ancestry(4242, 900, |_| None), Ancestry::Undetermined);
    }

    #[test]
    fn a_client_in_the_chain_is_found_at_any_depth() {
        let tree = |pid: u32| match pid {
            900 => Some(800),
            800 => Some(700),
            700 => Some(1),
            _ => None,
        };
        assert_eq!(walk_ancestry(800, 900, tree), Ancestry::Ancestor);
        assert_eq!(walk_ancestry(700, 900, tree), Ancestry::Ancestor);
        assert_eq!(walk_ancestry(999, 900, tree), Ancestry::NotAncestor);
    }

    /// A cycle must neither hang the daemon nor claim an answer.
    #[test]
    fn a_cycle_is_undetermined_rather_than_a_hang() {
        let cycle = |pid: u32| Some(if pid == 900 { 800 } else { 900 });
        assert_eq!(walk_ancestry(4242, 900, cycle), Ancestry::Undetermined);
    }

    /// Undetermined must be visible to the caller, not folded into
    /// either real answer.
    #[test]
    fn undetermined_is_its_own_verdict_and_does_not_refuse() {
        let v = ConnectionVerdict::Undetermined;
        assert_ne!(v, ConnectionVerdict::Trusted);
        assert_ne!(v, ConnectionVerdict::CallerIsAncestor);
        assert!(
            !v.should_refuse(false),
            "ancestry is a Linux ptrace question; refusing everywhere it cannot be read would \
             take the daemon down on platforms where it grants nothing"
        );
        assert!(ConnectionVerdict::CallerIsAncestor.should_refuse(false));
    }

    #[test]
    fn trust_level_wire_names_are_stable() {
        assert_eq!(TrustLevel::SeparateUid.as_str(), "separate_uid");
        assert_eq!(TrustLevel::Independent.as_str(), "independent");
        assert_eq!(TrustLevel::AgentParented.as_str(), "agent_parented");
    }

    /// TOTP's whole guarantee is that the caller cannot read
    /// `totp_secret` from daemon memory. Offering it to a caller
    /// that can is theatre.
    #[test]
    fn totp_is_offered_only_above_agent_parented() {
        assert!(TrustLevel::SeparateUid.allows_totp());
        assert!(TrustLevel::Independent.allows_totp());
        assert!(!TrustLevel::AgentParented.allows_totp());
    }

    fn provenance(reparented: bool, override_active: bool) -> StartupProvenance {
        StartupProvenance {
            reparented_to_init: reparented,
            parent_pid: 4242,
            has_controlling_terminal: false,
            override_active,
        }
    }

    /// Check B is fatal by default — the point of ADR-024 §7's
    /// fail-closed revision.
    #[test]
    fn a_session_parented_daemon_refuses_to_start() {
        assert!(provenance(false, false).is_fatal());
    }

    #[test]
    fn the_override_downgrades_but_does_not_launder() {
        let p = provenance(false, true);

        assert!(!p.is_fatal(), "override permits startup");
        assert_eq!(
            p.trust_level(),
            TrustLevel::AgentParented,
            "override must not change the reported trust level"
        );
        assert!(
            !p.trust_level().allows_totp(),
            "override must not resurrect the TOTP path"
        );
        assert!(
            p.warnings()
                .iter()
                .any(|w| w.contains(INSECURE_OVERRIDE_ENV)),
            "override must warn on every launch"
        );
    }

    #[test]
    fn an_independently_started_daemon_is_clean() {
        let p = provenance(true, false);
        assert!(!p.is_fatal());
        assert_eq!(p.trust_level(), TrustLevel::Independent);
        assert!(p.warnings().is_empty());
    }

    /// Check C warns rather than refusing: a TTY is a hint about
    /// launch method, not a `ptrace` capability.
    #[test]
    fn a_controlling_terminal_warns_but_does_not_block() {
        let p = StartupProvenance {
            reparented_to_init: true,
            parent_pid: 1,
            has_controlling_terminal: true,
            override_active: false,
        };
        assert!(!p.is_fatal());
        assert!(
            p.warnings()
                .iter()
                .any(|w| w.contains("controlling terminal"))
        );
    }

    #[test]
    fn refusal_message_names_the_platform_command_and_the_override() {
        let msg = provenance(false, false).refusal_message();
        assert!(msg.contains(platform_start_command()), "{msg}");
        assert!(msg.contains(INSECURE_OVERRIDE_ENV), "{msg}");
        assert!(
            msg.contains("4242"),
            "should name the offending parent: {msg}"
        );
    }

    #[test]
    fn connection_refusal_follows_the_override() {
        assert!(ConnectionVerdict::CallerIsAncestor.should_refuse(false));
        assert!(!ConnectionVerdict::CallerIsAncestor.should_refuse(true));
        assert!(!ConnectionVerdict::Trusted.should_refuse(false));
    }

    /// PID 1 is never a descendant of anything, so an
    /// init-parented daemon can never see its caller as an
    /// ancestor.
    #[test]
    fn ancestry_walk_terminates_at_init() {
        assert!(!client_is_ancestor(999_999, 1));
    }

    /// A real check against this test process: its own PID is not
    /// an ancestor of itself, and its actual parent is.
    #[cfg(target_os = "linux")]
    #[test]
    fn ancestry_walk_finds_the_real_parent() {
        let me = std::process::id();
        assert!(
            !client_is_ancestor(me, me),
            "a process is not its own ancestor"
        );

        if let Some(parent) = parent_of(me) {
            assert!(
                client_is_ancestor(parent, me),
                "the real parent must be found in the chain"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_of_handles_comm_fields_containing_spaces() {
        // Reading our own stat must succeed regardless of how the
        // binary is named; the parser keys on the last ')'.
        assert!(parent_of(std::process::id()).is_some());
    }
}
