//! Explicit CI-mode detection per [ADR-021] §8 ("CI mode (explicit,
//! not heuristic)").
//!
//! ADR-021 deliberately separates *selection* from *detection*: CI
//! behaviour is selected (env var, context flag, or `--ci`), and CI
//! signals are merely *observed* and reported via `doctor`. This
//! avoids two failure modes the previous draft suffered:
//!
//! - A developer running a CI script locally with `CI=1` exported
//!   silently switching to env-store routing.
//! - A CI runner that does not set the expected variables silently
//!   staying on interactive routing and failing on the first
//!   keychain-unlock prompt.
//!
//! This module is the data layer:
//!
//! - [`detect_ci_mode`] reads the explicit signals + the heuristic
//!   signals and returns a [`CiDetection`].
//! - [`CiPolicy`] expresses the *behavioural* rules CI mode flips on
//!   (env-store first, skip `NotInstalled` silently, refuse
//!   local-vault unlock, refuse `BIOMETRIC_PROMPT`).
//!
//! The actual orchestration that consumes the policy lives one
//! layer up (the router, P11+). Splitting detection from
//! orchestration keeps both halves trivial to unit test.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

/// Env var that explicitly opts into CI mode. Accepts `1` /
/// `true` (case-insensitive) as truthy; `0` / `false` (or unset)
/// as falsy.
pub const DEVBOY_CI_ENV: &str = "DEVBOY_CI";

/// Common env vars CI runners set. Their presence is *only* a
/// heuristic signal — the router does not flip CI mode on them.
/// Per ADR-021 §8 the only effect of these is the `doctor` notice
/// when none of the explicit triggers fire.
pub const CI_HEURISTIC_VARS: &[&str] = &["CI", "GITLAB_CI", "GITHUB_ACTIONS", "BUILDKITE"];

// =============================================================================
// Detection result
// =============================================================================

/// How CI mode was activated. `None` when CI mode is inactive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiActivation {
    /// `--ci` was passed on the CLI.
    CliFlag,
    /// `DEVBOY_CI=<truthy>` was set in the environment.
    EnvVar {
        /// The verbatim value of `DEVBOY_CI` so logs can echo it.
        value: String,
    },
    /// The active context declares `[runtime] ci = true`.
    ContextConfig,
}

/// Outcome of [`detect_ci_mode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiDetection {
    /// `true` iff CI mode is selected.
    pub active: bool,
    /// How CI mode was activated. `None` when inactive.
    pub activation: Option<CiActivation>,
    /// Heuristic signals (`CI`, `GITLAB_CI`, …) observed in the
    /// environment. Reported regardless of the active state so
    /// `doctor` can show "we saw CI signals, but you didn't opt
    /// in."
    pub heuristic_signals: Vec<String>,
}

impl CiDetection {
    /// `true` when heuristic CI signals were observed but the user
    /// did *not* opt in via any explicit trigger. Drives the
    /// `doctor` notice.
    pub fn heuristic_without_explicit(&self) -> bool {
        !self.active && !self.heuristic_signals.is_empty()
    }

    /// Human-readable warning for `doctor` when heuristic signals
    /// fire without an explicit opt-in. Returns `None` otherwise.
    pub fn doctor_notice(&self) -> Option<String> {
        if !self.heuristic_without_explicit() {
            return None;
        }
        Some(format!(
            "CI signals detected ({signals}) — but `{env}` is not set; routing falls back to interactive defaults. \
             Pass `--ci` or export `{env}=1` to switch to CI routing.",
            signals = self.heuristic_signals.join(", "),
            env = DEVBOY_CI_ENV,
        ))
    }
}

// =============================================================================
// Detection
// =============================================================================

/// Detect whether CI mode is active.
///
/// Priority:
///
/// 1. `cli_flag` — `--ci` on the command line.
/// 2. `DEVBOY_CI=<truthy>` in the environment.
/// 3. `context_ci` — `[runtime] ci = true` from the active context.
///
/// Heuristic signals (`CI`, `GITLAB_CI`, `GITHUB_ACTIONS`,
/// `BUILDKITE`) are recorded in the result but *do not* flip CI
/// mode on their own. This is the explicit/heuristic split the
/// ADR insists on.
///
/// `cli_flag = false` and `context_ci = None` are the "no opinion"
/// neutral inputs; CLI / context loaders pass concrete values
/// when available.
pub fn detect_ci_mode(cli_flag: bool, context_ci: Option<bool>) -> CiDetection {
    let heuristic_signals = collect_heuristic_signals();
    let env_active = read_explicit_env_value();
    let activation = if cli_flag {
        Some(CiActivation::CliFlag)
    } else if let Some(value) = env_active {
        Some(CiActivation::EnvVar { value })
    } else if context_ci.unwrap_or(false) {
        Some(CiActivation::ContextConfig)
    } else {
        None
    };
    CiDetection {
        active: activation.is_some(),
        activation,
        heuristic_signals,
    }
}

/// Read `DEVBOY_CI` and return the verbatim value when truthy,
/// `None` otherwise.
fn read_explicit_env_value() -> Option<String> {
    let raw = std::env::var(DEVBOY_CI_ENV).ok()?;
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_lowercase();
    if lower == "1" || lower == "true" {
        Some(raw)
    } else {
        None
    }
}

/// Collect heuristic CI vars currently set in the environment.
fn collect_heuristic_signals() -> Vec<String> {
    let mut out = Vec::new();
    for var in CI_HEURISTIC_VARS {
        if let Ok(v) = std::env::var(var)
            && is_truthy(&v)
        {
            out.push((*var).to_owned());
        }
    }
    out
}

fn is_truthy(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let lower = s.to_lowercase();
    !(lower == "0" || lower == "false")
}

// =============================================================================
// Policy
// =============================================================================

/// Behavioural rules CI mode flips on, per ADR-021 §8.
///
/// The router (P11+) consumes this and applies each flag at the
/// matching decision point: which source to consult first, whether
/// to emit a fallback warning, whether to invoke an unlock UI, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CiPolicy {
    /// Promote `env-store` to the front of the resolution chain
    /// regardless of the routing table.
    pub prefer_env_store: bool,
    /// Skip sources whose `is_available()` returns
    /// [`SourceStatus::NotInstalled`](crate::source::SourceStatus::NotInstalled)
    /// without surfacing the fallback as a warning. In interactive
    /// mode the same case logs a `doctor` notice.
    pub skip_not_installed_silently: bool,
    /// Refuse to invoke the `local-vault` unlock UI — no PIN or
    /// passphrase prompts in CI.
    pub refuse_local_vault_unlock: bool,
    /// Skip sources that declare
    /// [`Capabilities::BIOMETRIC_PROMPT`](crate::source::Capabilities::BIOMETRIC_PROMPT)
    /// — biometric unlock makes no sense in headless CI.
    pub refuse_biometric_sources: bool,
    /// Emit routing decisions at `info` level so a CI pipeline can
    /// grep for surprises. Interactive mode uses `debug` so the
    /// developer's terminal stays quiet.
    pub emit_decisions_as_info: bool,
}

impl CiPolicy {
    /// Policy when CI mode is active. Every field is the strict
    /// CI behaviour from ADR-021 §8.
    pub fn active() -> Self {
        Self {
            prefer_env_store: true,
            skip_not_installed_silently: true,
            refuse_local_vault_unlock: true,
            refuse_biometric_sources: true,
            emit_decisions_as_info: true,
        }
    }

    /// Policy when CI mode is inactive — interactive defaults.
    pub fn inactive() -> Self {
        Self {
            prefer_env_store: false,
            skip_not_installed_silently: false,
            refuse_local_vault_unlock: false,
            refuse_biometric_sources: false,
            emit_decisions_as_info: false,
        }
    }
}

impl From<&CiDetection> for CiPolicy {
    fn from(d: &CiDetection) -> Self {
        if d.active {
            Self::active()
        } else {
            Self::inactive()
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `f` with all three explicit triggers off and the
    /// heuristic vars cleared, so each test starts from a known
    /// neutral environment.
    fn neutral_env<F: FnOnce()>(f: F) {
        let clears: Vec<(&str, Option<&str>)> = std::iter::once((DEVBOY_CI_ENV, None))
            .chain(CI_HEURISTIC_VARS.iter().map(|v| (*v, None)))
            .collect();
        temp_env::with_vars(clears, f);
    }

    fn with_env<F: FnOnce()>(extra: Vec<(&str, Option<&str>)>, f: F) {
        let mut all: Vec<(&str, Option<&str>)> = std::iter::once((DEVBOY_CI_ENV, None))
            .chain(CI_HEURISTIC_VARS.iter().map(|v| (*v, None)))
            .collect();
        all.extend(extra);
        temp_env::with_vars(all, f);
    }

    // -- Inactive baseline -----------------------------------------

    #[test]
    fn empty_environment_is_inactive_with_no_heuristics() {
        neutral_env(|| {
            let d = detect_ci_mode(false, None);
            assert!(!d.active);
            assert!(d.activation.is_none());
            assert!(d.heuristic_signals.is_empty());
            assert!(!d.heuristic_without_explicit());
            assert!(d.doctor_notice().is_none());
        });
    }

    // -- Explicit triggers -----------------------------------------

    #[test]
    fn devboy_ci_eq_1_activates() {
        with_env(vec![(DEVBOY_CI_ENV, Some("1"))], || {
            let d = detect_ci_mode(false, None);
            assert!(d.active);
            assert_eq!(
                d.activation,
                Some(CiActivation::EnvVar { value: "1".into() })
            );
        });
    }

    #[test]
    fn devboy_ci_eq_true_case_insensitive_activates() {
        for v in &["true", "TRUE", "True"] {
            with_env(vec![(DEVBOY_CI_ENV, Some(v))], || {
                let d = detect_ci_mode(false, None);
                assert!(d.active, "expected active for DEVBOY_CI={v:?}");
            });
        }
    }

    #[test]
    fn devboy_ci_eq_0_or_false_does_not_activate() {
        for v in &["0", "false", "FALSE", ""] {
            with_env(vec![(DEVBOY_CI_ENV, Some(v))], || {
                let d = detect_ci_mode(false, None);
                assert!(
                    !d.active,
                    "expected inactive for DEVBOY_CI={v:?}, got {d:?}"
                );
            });
        }
    }

    #[test]
    fn cli_flag_activates() {
        neutral_env(|| {
            let d = detect_ci_mode(true, None);
            assert!(d.active);
            assert_eq!(d.activation, Some(CiActivation::CliFlag));
        });
    }

    #[test]
    fn context_config_activates() {
        neutral_env(|| {
            let d = detect_ci_mode(false, Some(true));
            assert!(d.active);
            assert_eq!(d.activation, Some(CiActivation::ContextConfig));
        });
    }

    #[test]
    fn context_config_false_does_not_activate() {
        neutral_env(|| {
            let d = detect_ci_mode(false, Some(false));
            assert!(!d.active);
        });
    }

    // -- Priority --------------------------------------------------

    #[test]
    fn cli_flag_wins_over_env_var() {
        with_env(vec![(DEVBOY_CI_ENV, Some("1"))], || {
            let d = detect_ci_mode(true, None);
            assert_eq!(d.activation, Some(CiActivation::CliFlag));
        });
    }

    #[test]
    fn env_var_wins_over_context() {
        with_env(vec![(DEVBOY_CI_ENV, Some("1"))], || {
            let d = detect_ci_mode(false, Some(true));
            assert_eq!(
                d.activation,
                Some(CiActivation::EnvVar { value: "1".into() })
            );
        });
    }

    // -- Heuristic detection ---------------------------------------

    #[test]
    fn ci_eq_true_alone_is_heuristic_signal_only() {
        with_env(vec![("CI", Some("true"))], || {
            let d = detect_ci_mode(false, None);
            assert!(!d.active, "CI alone must NOT flip CI mode");
            assert_eq!(d.heuristic_signals, vec!["CI".to_owned()]);
            assert!(d.heuristic_without_explicit());
            let notice = d.doctor_notice().unwrap();
            assert!(notice.contains("CI signals detected"));
            assert!(notice.contains("CI"));
            assert!(notice.contains(DEVBOY_CI_ENV));
        });
    }

    #[test]
    fn ci_eq_false_does_not_count_as_signal() {
        with_env(vec![("CI", Some("false"))], || {
            let d = detect_ci_mode(false, None);
            assert!(d.heuristic_signals.is_empty());
        });
    }

    #[test]
    fn each_heuristic_var_is_recognised() {
        for var in CI_HEURISTIC_VARS {
            with_env(vec![(var, Some("1"))], || {
                let d = detect_ci_mode(false, None);
                assert!(
                    d.heuristic_signals.contains(&(*var).to_owned()),
                    "expected {var} to be a recognised heuristic, got signals {:?}",
                    d.heuristic_signals,
                );
            });
        }
    }

    #[test]
    fn explicit_trigger_silences_doctor_notice_even_with_heuristics() {
        with_env(
            vec![
                (DEVBOY_CI_ENV, Some("1")),
                ("CI", Some("true")),
                ("GITHUB_ACTIONS", Some("true")),
            ],
            || {
                let d = detect_ci_mode(false, None);
                assert!(d.active);
                // Heuristics are still listed for transparency...
                assert!(d.heuristic_signals.contains(&"CI".into()));
                assert!(d.heuristic_signals.contains(&"GITHUB_ACTIONS".into()));
                // ...but the doctor notice fires only when there's
                // no explicit trigger.
                assert!(!d.heuristic_without_explicit());
                assert!(d.doctor_notice().is_none());
            },
        );
    }

    // -- Policy ----------------------------------------------------

    #[test]
    fn ci_policy_active_flips_every_rule() {
        let p = CiPolicy::active();
        assert!(p.prefer_env_store);
        assert!(p.skip_not_installed_silently);
        assert!(p.refuse_local_vault_unlock);
        assert!(p.refuse_biometric_sources);
        assert!(p.emit_decisions_as_info);
    }

    #[test]
    fn ci_policy_inactive_is_all_off() {
        let p = CiPolicy::inactive();
        assert!(!p.prefer_env_store);
        assert!(!p.skip_not_installed_silently);
        assert!(!p.refuse_local_vault_unlock);
        assert!(!p.refuse_biometric_sources);
        assert!(!p.emit_decisions_as_info);
    }

    #[test]
    fn ci_policy_from_detection_picks_active_when_active() {
        let active = CiDetection {
            active: true,
            activation: Some(CiActivation::CliFlag),
            heuristic_signals: vec![],
        };
        assert_eq!(CiPolicy::from(&active), CiPolicy::active());
    }

    #[test]
    fn ci_policy_from_detection_picks_inactive_when_inactive() {
        let inactive = CiDetection {
            active: false,
            activation: None,
            heuristic_signals: vec!["CI".into()],
        };
        assert_eq!(CiPolicy::from(&inactive), CiPolicy::inactive());
    }
}
