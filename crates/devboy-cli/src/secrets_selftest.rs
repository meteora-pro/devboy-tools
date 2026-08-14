//! `devboy secrets selftest` — what the framework is *actually*
//! doing right now (T7 / ADR-024).
//!
//! `doctor` answers "is anything broken". This answers a different
//! question: **which posture is in force, and where is it weaker
//! than it looks.** Those come apart constantly in this framework
//! — a vault can be perfectly healthy while running with the TOTP
//! path unavailable, a `strict` profile that cannot prompt, or a
//! daemon whose memory the caller can read. None of that is a
//! fault; all of it changes what the guarantees mean.
//!
//! Everything printed is derived from live state rather than from
//! configuration, because the two disagree exactly when it matters.

use anyhow::Result;
use clap::Args;
use devboy_core::config::Config;
use devboy_mcp::remediation::{ALL_ERROR_KINDS, RemediationActor, RemediationContext};
use devboy_secret_env_store::candidate_env_names;

/// Arguments for `devboy secrets selftest`.
#[derive(Args, Debug, Default)]
pub struct SelftestArgs {
    /// Emit JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,

    /// Check the environment variables that would satisfy this
    /// path, under both naming conventions.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,
}

/// One line of the report.
struct Finding {
    label: &'static str,
    value: String,
    /// Why this matters, when the value alone would not say.
    note: Option<String>,
}

impl Finding {
    fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            note: None,
        }
    }

    fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// Run the selftest.
pub fn handle(args: SelftestArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let mut findings = Vec::new();

    findings.extend(resolution_findings(&config));
    findings.extend(profile_findings(&config));
    findings.extend(trust_level_findings());
    findings.extend(storage_findings(&config));
    findings.extend(agent_protocol_findings());

    if let Some(path) = args.path.as_deref() {
        findings.extend(path_findings(path));
    }

    if args.json {
        let obj: serde_json::Map<String, serde_json::Value> = findings
            .iter()
            .map(|f| {
                (
                    f.label.to_string(),
                    serde_json::json!({ "value": f.value, "note": f.note }),
                )
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    println!("DevBoy secrets selftest");
    println!("=======================\n");

    let width = findings.iter().map(|f| f.label.len()).max().unwrap_or(0);
    for finding in &findings {
        println!("  {:<width$}  {}", finding.label, finding.value);
        if let Some(note) = &finding.note {
            println!("  {:<width$}  ↳ {note}", "");
        }
    }

    println!();
    Ok(())
}

/// What the daemon's state costs, in words.
///
/// Split from [`resolution_findings`] because the no-socket case
/// never fires on a developer's own machine — it is the platform
/// without UNIX sockets, and the environment with no config
/// directory. Reached only through the real store, that branch
/// would be written blind and verified by a CI runner, which is
/// how it came to explain the cause and forget the consequence.
fn daemon_note(socket: Option<&std::path::Path>, running: bool) -> String {
    match socket {
        Some(path) if running => format!("socket {}", path.display()),
        Some(path) => format!(
            "no socket at {} — every secret held in the vault is currently unreachable, and \
             will report as simply not found. Start it with `devboy secrets agent start`.",
            path.display()
        ),
        None => format!(
            "{WHY_NO_SOCKET} — every secret held in the vault is therefore unreachable and \
             will report as simply not found. Supply those secrets through the environment \
             instead."
        ),
    }
}

/// Why no daemon socket path exists, when none does.
///
/// The two reasons are not the same problem. Off UNIX there is
/// no daemon to have a socket, which is a property of the
/// platform and nothing the user can fix. On UNIX it means the
/// config directory could not be derived, which is a broken
/// environment.
///
/// Either way the *consequence* is identical and is what the
/// report leads with: vault-held secrets read as absent rather
/// than as unreachable, which is the one failure this command
/// exists to make visible.
#[cfg(unix)]
const WHY_NO_SOCKET: &str = "no socket path could be derived on this machine";
#[cfg(not(unix))]
const WHY_NO_SOCKET: &str =
    "the vault daemon speaks over a UNIX domain socket, which this platform does not have";

/// Where secrets come from, and whether anything is implicit.
fn resolution_findings(config: &Config) -> Vec<Finding> {
    let detection = devboy_storage::detect_ci_mode(false, Some(config.is_ci_forced()));
    let keychain = config.is_keychain_enabled();

    let mode = if detection.active {
        "env-only (CI)"
    } else if keychain {
        "env → local vault → OS keychain"
    } else {
        "env → local vault"
    };

    let mut out = vec![Finding::new("resolution", mode)];

    // The chain lists the vault whether or not the daemon is up,
    // and a stopped daemon is invisible in normal use: the lookup
    // simply returns nothing, exactly as if the secret had never
    // been stored. That is the "healthy but weaker than it looks"
    // case this command exists for.
    if !detection.active {
        let vault = devboy_secret_local_vault::VaultStore::new();
        let running = vault.as_ref().is_some_and(|v| v.daemon_present());
        out.push(
            Finding::new(
                "vault daemon",
                if running { "running" } else { "not running" },
            )
            .with_note(daemon_note(
                vault.as_ref().map(|v| v.socket_path()),
                running,
            )),
        );
    }

    if !detection.active && !keychain {
        out.push(Finding::new("keychain", "disabled (default)").with_note(
            "ADR-024 §6: the OS keychain only exceeds chmod 0600 on macOS. Enable with \
                 `devboy config set secrets.keychain.enabled true` if you keep tokens there.",
        ));
    } else if keychain {
        out.push(Finding::new("keychain", "enabled"));
    }

    // The interesting case: something smells like CI but nothing
    // switched the mode.
    if let Some(notice) = detection.doctor_notice() {
        out.push(Finding::new("ci heuristic", "detected, mode unchanged").with_note(notice));
    }

    out
}

/// The unlock window, and whether the chosen profile can actually
/// be honoured here.
fn profile_findings(config: &Config) -> Vec<Finding> {
    let profile = config.secrets_profile();
    let mut out = vec![
        Finding::new(
            "profile",
            config
                .get("secrets.profile")
                .ok()
                .flatten()
                .unwrap_or_else(|| "convenient".to_string()),
        ),
        Finding::new(
            "unlock window",
            format!(
                "{}s (ceiling {}s)",
                config.unlock_ttl_seconds(),
                config.max_unlock_ttl_seconds()
            ),
        )
        .with_note("as configured — see `window in force` for what the daemon is enforcing"),
        Finding::new(
            "idle re-lock",
            config
                .idle_relock_seconds()
                .map(|s| format!("{s}s"))
                .unwrap_or_else(|| "off".to_string()),
        ),
    ];

    // The configured window and the enforced one are different
    // facts, and they diverge for an ordinary reason: the daemon
    // reads config once, at startup. Printing the configured number
    // alone is how this command would mislead someone into thinking
    // a tightened policy had taken effect.
    out.push(enforced_window_finding(config));

    // `strict` promises per-call approval, which needs someone to
    // ask. Saying so here is the point of the command.
    if profile.requires_prompt_surface() {
        let can_prompt = std::io::IsTerminal::is_terminal(&std::io::stdin());
        out.push(
            Finding::new(
                "prompt surface",
                if can_prompt { "available" } else { "MISSING" },
            )
            .with_note(if can_prompt {
                "the `strict` profile can ask for per-call approval".to_string()
            } else {
                "the `strict` profile forces per-call approval, but there is nobody to ask here \
                 — it will fail at the first secret access"
                    .to_string()
            }),
        );
    }

    for warning in config.secrets_config_warnings() {
        out.push(Finding::new("config warning", warning));
    }

    out
}

/// Plain-language note for a trust level the daemon reported.
///
/// The arms are keyed off [`TrustLevel::as_str`] rather than
/// hand-written strings. They used to be literals with hyphens —
/// `"agent-parented"`, `"separate-uid"` — while the daemon sends
/// underscores, so those two arms never matched and the command
/// whose whole job is "what am I actually getting" answered "the
/// daemon did not report a recognised trust level" to the two
/// people who most needed the explanation.
fn trust_level_note(level: &str) -> &'static str {
    use devboy_secrets_agent::provenance::TrustLevel::*;

    if level == AgentParented.as_str() {
        "the daemon was started by its caller, which can therefore trace it and read the vault \
         key out of its memory. The TOTP path is disabled here because a code would prove nothing."
    } else if level == PtraceUnrestricted.as_str() {
        "the daemon runs outside its caller's process tree, but this kernel does not restrict \
         ptrace between processes of the same user — so anything running as you can read its \
         memory anyway. Fix with `sudo sysctl -w kernel.yama.ptrace_scope=1`. TOTP is disabled \
         until then."
    } else if level == Independent.as_str() {
        "the daemon runs outside its caller's process tree and the kernel restricts ptrace to \
         descendants, so the caller cannot trace it — but it still runs as your user, so anything \
         that can read your files can read the vault file."
    } else if level == SeparateUid.as_str() {
        "the daemon runs under its own account, so the vault file is not readable by your user \
         directly."
    } else {
        "the daemon did not report a recognised trust level"
    }
}

/// Which §7 trust level is actually in force, and whether the
/// prompt channel the level assumes exists.
///
/// ADR-024 §7 distinguishes its levels by who owns the input
/// channel, so a report that names a level without naming the
/// channel is describing an intention rather than a state.
///
/// This is also where the framework's own contradiction becomes
/// visible: §7 wants the daemon reparented to init *and* prompting
/// on its own terminal, and a reparented process has no terminal.
/// A user finding that out here is far better off than one finding
/// it out at their first unlock.
fn trust_level_findings() -> Vec<Finding> {
    let Some(status) = daemon_status() else {
        return vec![Finding::new("trust level", "unknown — daemon not running")];
    };

    let level = status
        .get("trust_level")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let channel = status
        .get("prompt_channel")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let override_active = status
        .get("insecure_override")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut out =
        vec![Finding::new("trust level", level.to_owned()).with_note(trust_level_note(level))];

    let mut channel_finding = Finding::new(
        "prompt channel",
        match channel {
            "terminal" => "the daemon's own terminal",
            "none" => "none",
            other => other,
        },
    );
    if channel == "none" {
        channel_finding = channel_finding.with_note(
            "the daemon has no terminal of its own. This is the normal state for a \
             properly-installed service — the same startup check that makes it trustworthy is \
             what leaves it without one — and it is no longer a dead end: `devboy secrets agent \
             unlock` lends the daemon the terminal you run it from, and the prompt appears \
             there. For an unattended start, export DEVBOY_VAULT_PASSPHRASE instead.",
        );
    } else if let Some(daemon_tty) = status.get("terminal_id")
        && !daemon_tty.is_null()
        && let Some(mine) = current_terminal_id()
        && daemon_tty == &serde_json::json!([mine.0, mine.1])
    {
        // Both on one terminal means whoever else has it open can
        // read what is typed, which is the entire thing the move
        // was supposed to prevent.
        channel_finding = channel_finding.with_note(
            "the daemon shares THIS terminal, so moving the prompt into it buys nothing —              anything attached here can read what you type.",
        );
    }
    out.push(channel_finding);

    if override_active {
        out.push(
            Finding::new("insecure override", "ACTIVE").with_note(
                "DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON is set, so the daemon started despite                  failing its own trust check. Intended for tests; unset it for real use.",
            ),
        );
    }

    out
}

/// This process's controlling terminal, for comparison with the
/// daemon's.
#[cfg(unix)]
fn current_terminal_id() -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata("/dev/tty").ok()?;
    Some((meta.rdev(), meta.ino()))
}

#[cfg(not(unix))]
fn current_terminal_id() -> Option<(u64, u64)> {
    None
}

/// What the running daemon is *actually* enforcing, asked of the
/// daemon rather than inferred from config.
///
/// The daemon reads config once at startup, so a config edit does
/// not reach a daemon that is already running. Reporting the
/// configured number as if it were live is exactly how this command
/// would mislead the person it exists to inform.
fn enforced_window_finding(config: &Config) -> Finding {
    let Some(status) = daemon_status() else {
        return Finding::new("window in force", "unknown — daemon not running").with_note(
            "nothing is enforcing a window right now; it takes effect when the daemon starts",
        );
    };

    let live_ttl = status.get("unlock_ttl_seconds").and_then(|v| v.as_u64());
    let live_ceiling = status
        .get("max_unlock_ttl_seconds")
        .and_then(|v| v.as_u64());

    let (Some(live_ttl), Some(live_ceiling)) = (live_ttl, live_ceiling) else {
        return Finding::new("window in force", "unknown — daemon did not report one")
            .with_note("the daemon predates this reporting; restart it to see the live window");
    };

    let configured_ttl = config.unlock_ttl_seconds();
    let configured_ceiling = config.max_unlock_ttl_seconds();
    let matches = live_ttl == configured_ttl && live_ceiling == configured_ceiling;

    let finding = Finding::new(
        "window in force",
        format!("{live_ttl}s (ceiling {live_ceiling}s)"),
    );

    if matches {
        finding
    } else {
        finding.with_note(format!(
            "DIFFERS from the configured {configured_ttl}s / {configured_ceiling}s — the daemon \
             read its policy at startup and has not seen the change. Restart it with `devboy \
             secrets agent start` for the new window to take effect."
        ))
    }
}

/// Ask the daemon for its status, or `None` if it is not answering.
fn daemon_status() -> Option<serde_json::Value> {
    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let socket = devboy_secrets_agent::default_socket_path().ok()?;
        let stream = UnixStream::connect(socket).ok()?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .ok();

        let mut writer = stream.try_clone().ok()?;
        writeln!(
            writer,
            r#"{{"jsonrpc":"2.0","id":1,"method":"vault.status","params":null}}"#
        )
        .ok()?;
        writer.flush().ok()?;
        drop(writer);

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).ok()?;
        let response: serde_json::Value = serde_json::from_str(&line).ok()?;
        response.get("result").cloned()
    }
    #[cfg(not(unix))]
    {
        // The daemon speaks over a UNIX domain socket, so off
        // UNIX there is nothing to ask.
        None
    }
}

/// Where things live on disk, and whether the split that makes a
/// keyfile worth having is actually in place.
fn storage_findings(config: &Config) -> Vec<Finding> {
    let mut out = Vec::new();

    match config.secrets_keyfile_path() {
        Some(path) => {
            let exists = path.exists();
            let mut finding = Finding::new(
                "keyfile",
                format!(
                    "{} ({})",
                    path.display(),
                    if exists { "present" } else { "missing" }
                ),
            );

            // The whole value of a keyfile is that a backup of one
            // half does not carry the other.
            if let Some(config_dir) = dirs::config_dir()
                && path.starts_with(&config_dir)
            {
                finding = finding.with_note(
                    "this keyfile lives under the config directory alongside the vault, so a \
                     single backup carries both halves and the split buys nothing",
                );
            }
            out.push(finding);
        }
        None => out.push(Finding::new("keyfile", "not configured")),
    }

    // `migration_complete` currently gates nothing: there is no
    // fallback reader for it to switch off. Saying otherwise — as
    // this line did — tells an upgrading user they are covered when
    // they are not, which is the worst thing this command can do.
    out.push(if config.is_secrets_migration_complete() {
        Finding::new("migration", "marked complete")
    } else {
        Finding::new("migration", "not marked complete").with_note(
            "this flag only affects reporting today — nothing reads the OS keychain as a \
             fallback. If you upgraded from 0.33 with tokens stored there, they no longer \
             resolve: re-enable the keychain with `devboy config set secrets.keychain.enabled \
             true`, or move them with `devboy secrets migrate`.",
        )
    });

    out
}

/// Self-check of the agent-facing error contract: every failure
/// must carry advice, and none may contradict itself.
fn agent_protocol_findings() -> Vec<Finding> {
    let ctx = RemediationContext::for_path("selftest/probe");
    let mut missing = Vec::new();
    let mut contradictory = Vec::new();

    for kind in ALL_ERROR_KINDS {
        let r = kind.remediation(&ctx);

        if r.user_message.trim().is_empty() {
            missing.push(format!("{kind:?}"));
        }
        // A human-only failure that invites a retry is what sends
        // an agent into a loop.
        if r.actor == RemediationActor::User && r.retryable {
            contradictory.push(format!("{kind:?}"));
        }
    }

    let total = ALL_ERROR_KINDS.len();
    let mut finding = Finding::new(
        "agent protocol",
        if missing.is_empty() && contradictory.is_empty() {
            format!("{total} error kinds, all actionable")
        } else {
            format!(
                "{total} error kinds, {} without advice, {} contradictory",
                missing.len(),
                contradictory.len()
            )
        },
    );

    if !contradictory.is_empty() {
        finding = finding.with_note(format!(
            "these would loop an agent: {}",
            contradictory.join(", ")
        ));
    }

    vec![finding]
}

/// Which environment variables would satisfy a specific path.
fn path_findings(path: &str) -> Vec<Finding> {
    let candidates = candidate_env_names(path, None);
    let set: Vec<String> = candidates
        .iter()
        .map(|name| {
            let present = std::env::var(name).is_ok();
            format!("{name}{}", if present { " ✓" } else { "" })
        })
        .collect();

    vec![
        Finding::new("path", path.to_owned()),
        Finding::new("env candidates", set.join(", ")).with_note(
            "checked in this order; both the ADR-021 convention name and the ADR-005 legacy \
             names are honoured",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent-protocol probe is the selftest's own regression
    /// check — if it ever reports a contradiction, an agent would
    /// loop in production.
    #[test]
    fn the_agent_protocol_probe_passes_against_the_real_contract() {
        let findings = agent_protocol_findings();
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].value.contains("all actionable"),
            "the shipped contract is not self-consistent: {} / {:?}",
            findings[0].value,
            findings[0].note
        );
    }

    #[test]
    fn resolution_reports_the_vault_chain_when_the_keychain_is_off() {
        let config = Config::default();
        let findings = resolution_findings(&config);

        let resolution = findings
            .iter()
            .find(|f| f.label == "resolution")
            .expect("resolution is reported");
        assert!(
            resolution.value.contains("vault"),
            "the vault is in the default chain and the report must say so: {}",
            resolution.value
        );

        let keychain = findings
            .iter()
            .find(|f| f.label == "keychain")
            .expect("keychain state is reported");
        assert!(keychain.value.contains("disabled"));
        assert!(
            keychain.note.is_some(),
            "a disabled keychain should explain how to turn it on"
        );
    }

    /// The three shapes of the daemon note. The point every one
    /// of them has to make is the *consequence*: a vault the
    /// process cannot reach reports its secrets as absent, which
    /// looks exactly like never having stored them.
    #[test]
    fn every_unreachable_daemon_note_says_what_it_costs() {
        let path = std::path::Path::new("/run/user/1000/devboy/agent.sock");

        let running = daemon_note(Some(path), true);
        assert!(running.contains("agent.sock"), "{running}");

        let stopped = daemon_note(Some(path), false);
        assert!(stopped.contains("unreachable"), "{stopped}");
        assert!(
            stopped.contains("secrets agent start"),
            "a stopped daemon has a fix: {stopped}"
        );

        // The branch a developer machine never takes: no socket
        // path at all. Off UNIX this is every run.
        let absent = daemon_note(None, false);
        assert!(
            absent.contains("unreachable"),
            "explaining the cause without the consequence is what broke this on Windows: \
             {absent}"
        );
        assert!(
            absent.contains("environment"),
            "with no daemon there has to be somewhere else to put the secret: {absent}"
        );
    }

    /// A stopped daemon makes every vault-held secret read as
    /// "not found", which is indistinguishable from never having
    /// stored it. Reporting the daemon's state is the only place a
    /// user finds out.
    #[test]
    fn resolution_reports_whether_the_vault_daemon_is_running() {
        let config = Config::default();
        let findings = resolution_findings(&config);

        let daemon = findings
            .iter()
            .find(|f| f.label == "vault daemon")
            .expect("daemon state is reported outside CI mode");

        if daemon.value.contains("not running") {
            assert!(
                daemon.note.as_deref().unwrap_or("").contains("unreachable"),
                "a stopped daemon must explain what it silently costs: {:?}",
                daemon.note
            );
        }
    }

    #[test]
    fn profile_findings_report_the_effective_window() {
        let mut config = Config::default();
        config.set("secrets.profile", "strict").unwrap();

        let findings = profile_findings(&config);
        let window = findings
            .iter()
            .find(|f| f.label == "unlock window")
            .expect("window is reported");

        assert!(window.value.contains("900"), "{}", window.value);
        assert!(window.value.contains("3600"), "{}", window.value);
    }

    /// A `strict` profile with nowhere to prompt is exactly the
    /// "healthy but weaker than it looks" case this command exists
    /// to surface.
    #[test]
    fn strict_profile_reports_whether_it_can_prompt_at_all() {
        let mut config = Config::default();
        config.set("secrets.profile", "strict").unwrap();

        let findings = profile_findings(&config);
        assert!(
            findings.iter().any(|f| f.label == "prompt surface"),
            "strict must report whether a prompt surface exists"
        );
    }

    #[test]
    fn convenient_profile_does_not_claim_to_need_a_prompt_surface() {
        let config = Config::default();
        let findings = profile_findings(&config);

        assert!(!findings.iter().any(|f| f.label == "prompt surface"));
    }

    #[test]
    fn config_warnings_are_surfaced() {
        let mut config = Config::default();
        config.set("secrets.max_unlock_ttl_seconds", "60").unwrap();
        config.set("secrets.unlock_ttl_seconds", "86400").unwrap();

        let findings = profile_findings(&config);
        assert!(
            findings.iter().any(|f| f.label == "config warning"),
            "a clamped window must be reported, not silently applied"
        );
    }

    #[test]
    fn path_findings_list_both_naming_conventions() {
        let findings = path_findings("team/gitlab/token");
        let candidates = findings
            .iter()
            .find(|f| f.label == "env candidates")
            .expect("candidates are listed");

        assert!(
            candidates
                .value
                .contains("DEVBOY_SECRET__TEAM__GITLAB__TOKEN")
        );
        assert!(candidates.value.contains("DEVBOY_GITLAB_TOKEN"));
        assert!(candidates.value.contains("GITLAB_TOKEN"));
    }

    /// With no daemon running, the trust level is unknown rather
    /// than guessed — claiming a level nothing is enforcing would
    /// be exactly the misreporting this command exists to avoid.
    /// Every level the daemon can report must get a real
    /// explanation.
    ///
    /// The arms were once string literals with hyphens while the
    /// daemon sends underscores, so `agent_parented` and
    /// `separate_uid` silently fell through to "not a recognised
    /// trust level". Driving the test from the enum means a level
    /// added later fails here instead of shipping unexplained.
    #[test]
    fn every_trust_level_the_daemon_can_report_is_explained() {
        use devboy_secrets_agent::provenance::TrustLevel;

        for level in [
            TrustLevel::SeparateUid,
            TrustLevel::Independent,
            TrustLevel::AgentParented,
            TrustLevel::PtraceUnrestricted,
        ] {
            let note = trust_level_note(level.as_str());
            assert!(
                !note.contains("did not report a recognised"),
                "`{}` falls through to the unknown arm",
                level.as_str()
            );
        }

        assert!(
            trust_level_note("something-new").contains("did not report a recognised"),
            "an unknown level should still be handled honestly"
        );
    }

    /// The open-ptrace note has to carry the fix, since that is the
    /// whole difference between it and `independent`.
    #[test]
    fn the_open_ptrace_note_names_the_sysctl() {
        use devboy_secrets_agent::provenance::TrustLevel;

        let note = trust_level_note(TrustLevel::PtraceUnrestricted.as_str());
        assert!(note.contains("kernel.yama.ptrace_scope=1"), "{note}");
    }

    #[test]
    fn trust_level_is_unknown_when_no_daemon_answers() {
        let findings = trust_level_findings();
        let level = findings
            .iter()
            .find(|f| f.label == "trust level")
            .expect("trust level is reported");

        // No daemon runs under the test harness.
        assert!(
            level.value.contains("unknown"),
            "expected an honest unknown, got {}",
            level.value
        );
    }

    /// The command must not crash or hang when the daemon is
    /// absent, which is the common case on a fresh machine.
    #[test]
    fn the_report_assembles_without_a_daemon() {
        let config = Config::default();
        let mut findings = Vec::new();
        findings.extend(resolution_findings(&config));
        findings.extend(profile_findings(&config));
        findings.extend(trust_level_findings());
        findings.extend(storage_findings(&config));
        findings.extend(agent_protocol_findings());

        assert!(findings.len() > 5, "the report should still say something");
        assert!(
            findings.iter().all(|f| !f.value.is_empty()),
            "every finding needs a value"
        );
    }

    /// A keyfile sharing a directory with the vault provides none
    /// of the separation it exists for, and the report should say
    /// so rather than list it as fine.
    #[test]
    fn a_keyfile_beside_the_vault_is_called_out() {
        let Some(config_dir) = dirs::config_dir() else {
            return;
        };

        let mut config = Config::default();
        config
            .set(
                "secrets.keyfile_path",
                config_dir.join("devboy-tools/vault.key").to_str().unwrap(),
            )
            .unwrap();

        let findings = storage_findings(&config);
        let keyfile = findings
            .iter()
            .find(|f| f.label == "keyfile")
            .expect("keyfile is reported");

        assert!(
            keyfile
                .note
                .as_deref()
                .unwrap_or("")
                .contains("both halves"),
            "a co-located keyfile must be flagged: {:?}",
            keyfile.note
        );
    }
}
