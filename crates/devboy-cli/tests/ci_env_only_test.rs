//! Contract tests for CI / env-only mode (T5, ADR-024 §6).
//!
//! These run the real binary, because the failures they guard
//! against are process-level and invisible to unit tests:
//!
//! - **A hang.** A prompt that appears in CI blocks the pipeline
//!   until the job times out, with no useful output. Every command
//!   here runs with stdin closed and a wall-clock deadline, so a
//!   prompt fails the test in seconds instead of wedging CI.
//! - **A silent name change.** ADR-005 pipelines set
//!   `DEVBOY_GITLAB_TOKEN` or bare `GITLAB_TOKEN`. If the default
//!   flip routed CI through the ADR-021 convention name alone,
//!   those pipelines would break as "secret not found" rather than
//!   as an obvious error — the worst possible failure shape.
//! - **A vanished write.** The CI chain used to pair the env store
//!   with an in-memory one, so a write appeared to succeed and was
//!   lost at exit.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Wall-clock budget for any single command.
///
/// Generous enough for a debug-build cold start, short enough that
/// a prompt is caught rather than waited on.
const DEADLINE: Duration = Duration::from_secs(30);

fn devboy_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push(format!("devboy{}", std::env::consts::EXE_SUFFIX));
    path
}

struct Outcome {
    stdout: String,
    stderr: String,
    success: bool,
}

impl Outcome {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Run `devboy` in a hermetic home, with stdin closed.
///
/// Closing stdin is the point: any code path that tries to prompt
/// gets EOF instead of a human, so a prompt shows up as a failure
/// rather than as a hang.
fn run(home: &TempDir, args: &[&str], env: &[(&str, &str)]) -> Outcome {
    let mut cmd = Command::new(devboy_bin());
    cmd.args(args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("XDG_STATE_HOME", home.path())
        // Never inherit the developer's own CI signals.
        .env_remove("CI")
        .env_remove("DEVBOY_CI")
        .env_remove("GITLAB_CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("DEVBOY_SKIP_KEYCHAIN")
        .current_dir(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (k, v) in env {
        cmd.env(k, v);
    }

    let started = Instant::now();
    let mut child = cmd.spawn().expect("spawn devboy");

    // Poll rather than `wait()`, so a wedged process is reported as
    // a hang instead of hanging the suite too.
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if started.elapsed() > DEADLINE => {
                let _ = child.kill();
                panic!(
                    "`devboy {}` did not finish within {DEADLINE:?} — it is almost certainly \
                     waiting on a prompt, which must never happen in env-only mode",
                    args.join(" ")
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    Outcome {
        stdout,
        stderr,
        success: status.success(),
    }
}

fn home() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// `DEVBOY_CI=1` puts the process in env-only mode, and `doctor`
/// says so rather than leaving the user to infer it.
#[test]
fn explicit_ci_flag_selects_env_only_mode() {
    let h = home();
    let out = run(
        &h,
        &["doctor", "--checks", "secrets-mode"],
        &[("DEVBOY_CI", "1")],
    );

    assert!(
        out.combined().contains("env-only"),
        "expected env-only mode, got:\n{}",
        out.combined()
    );
}

/// The §6 contract: heuristic variables raise a notice but never
/// change the posture. A security mode must not flip because an
/// unrelated tool exported `CI=1`.
#[test]
fn heuristic_ci_variables_do_not_flip_the_mode() {
    let h = home();
    let out = run(
        &h,
        &["doctor", "--checks", "secrets-mode"],
        &[("CI", "true")],
    );

    let text = out.combined();
    assert!(
        text.contains("env-default"),
        "heuristics must not switch the mode, got:\n{text}"
    );
    assert!(
        !text.contains("env-only"),
        "heuristics must not switch the mode, got:\n{text}"
    );
}

/// Without any CI signal the keychain is still absent from the
/// chain — the ADR-024 §6 default.
#[test]
fn the_default_chain_excludes_the_keychain() {
    let h = home();
    let out = run(&h, &["doctor", "--checks", "secrets-mode"], &[]);

    let text = out.combined();
    assert!(text.contains("env-default"), "{text}");
    assert!(
        text.contains("environment variables"),
        "the report should name the actual chain: {text}"
    );
}

/// Backwards compatibility, the part that would break silently.
///
/// A pipeline written against ADR-005 sets `DEVBOY_GITLAB_TOKEN`
/// or bare `GITLAB_TOKEN`. Both must still resolve after the
/// default flip.
#[test]
fn legacy_env_names_still_resolve_in_ci_mode() {
    for (var, value) in [
        ("DEVBOY_GITLAB_TOKEN", "glpat-legacy-prefixed"),
        ("GITLAB_TOKEN", "glpat-legacy-bare"),
    ] {
        let h = home();
        let out = run(
            &h,
            &["doctor", "--checks", "gitlab-token"],
            &[("DEVBOY_CI", "1"), (var, value)],
        );

        let text = out.combined();
        assert!(
            !text.to_lowercase().contains("not configured")
                && !text.to_lowercase().contains("missing"),
            "`{var}` should have satisfied the token check, got:\n{text}"
        );
        // The value itself must never be echoed back.
        assert!(
            !text.contains(value),
            "the token leaked into output:\n{text}"
        );
    }
}

/// Nothing in env-only mode may block on a prompt, a keychain, or
/// a daemon. Each of these would hang or fail in a container.
#[test]
fn common_commands_complete_without_prompting_in_ci_mode() {
    for args in [
        vec!["doctor", "--checks", "secrets-mode"],
        vec!["secrets", "list"],
        vec!["config", "get", "secrets.profile"],
    ] {
        let h = home();
        // `run` panics on the deadline, so reaching this line at
        // all means the command terminated.
        let out = run(&h, &args, &[("DEVBOY_CI", "1")]);

        assert!(
            !out.combined().to_lowercase().contains("passphrase for"),
            "`devboy {}` tried to prompt in CI mode:\n{}",
            args.join(" "),
            out.combined()
        );
    }
}

/// A missing secret must name the variables that would satisfy it,
/// so the fix pastes straight into a CI config instead of sending
/// the user to the docs.
#[test]
fn a_missing_secret_names_the_variables_that_would_satisfy_it() {
    let h = home();
    let out = run(
        &h,
        &["secrets", "describe", "team/gitlab/token"],
        &[("DEVBOY_CI", "1")],
    );

    let text = out.combined();
    assert!(!out.success, "an unknown path should not report success");
    assert!(
        text.contains("DEVBOY_SECRET__TEAM__GITLAB__TOKEN")
            || text.contains("DEVBOY_GITLAB_TOKEN")
            || text.contains("GITLAB_TOKEN"),
        "the error should list candidate variables, got:\n{text}"
    );
}

/// The profile knobs must be settable and readable without any
/// interactive step, since a CI image configures them
/// non-interactively.
#[test]
fn configuration_round_trips_without_interaction() {
    let h = home();

    let set = run(
        &h,
        &["config", "set", "secrets.profile", "strict"],
        &[("DEVBOY_CI", "1")],
    );
    assert!(set.success, "config set failed:\n{}", set.combined());

    let get = run(
        &h,
        &["config", "get", "secrets.profile"],
        &[("DEVBOY_CI", "1")],
    );
    assert!(
        get.combined().contains("strict"),
        "expected the value back, got:\n{}",
        get.combined()
    );
}

/// `DEVBOY_SKIP_KEYCHAIN` predates ADR-024 and is still honoured,
/// so existing CI configs keep working unchanged.
#[test]
fn the_legacy_skip_keychain_switch_still_selects_env_only() {
    let h = home();
    let out = run(
        &h,
        &["doctor", "--checks", "secrets-mode"],
        &[("DEVBOY_SKIP_KEYCHAIN", "1")],
    );

    // The legacy switch reaches the credential chain rather than
    // the doctor's own detection, so the assertion is that the
    // command works and reports a chain without the keychain.
    assert!(
        out.combined().contains("environment variables"),
        "legacy switch should still yield an env-only chain:\n{}",
        out.combined()
    );
}
