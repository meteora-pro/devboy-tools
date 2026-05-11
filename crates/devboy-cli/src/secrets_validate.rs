//! `devboy secrets validate` — CI gate for manifest paths per
//! [ADR-021] §6 ("the validation framework").
//!
//! Walks the active manifest (project + global index merged) and
//! checks each path against:
//!
//! - **Format** (default; P9.1): the candidate value matches the
//!   regex declared by `format_regex` or by the pattern referenced
//!   through `pattern_id`.
//! - **Liveness** (`--liveness`; P9.2): the candidate token is
//!   accepted by the upstream provider's introspection endpoint.
//!   Implemented for github + gitlab; other providers report
//!   `NotImplemented` (the ADR's "format-only validation only"
//!   path).
//!
//! Where do candidate values come from? The router orchestration
//! (P5+/P6/P11) is the long-term answer. Until that's wired
//! end-to-end, `validate` consults the env-store source's
//! conventional name (`DEVBOY_SECRET__<FLATTENED__PATH>`) so the
//! command is useful as a CI gate today: CI already exposes
//! tokens through env vars, and this command checks they match
//! the declared format / are live.
//!
//! ## Exit code
//!
//! Non-zero on any *hard* failure:
//!
//! - required path missing a value (env-store empty)
//! - format mismatch on a present value
//! - liveness reports `Revoked` or `Expired`
//!
//! Format `NoRule`, liveness `NotImplemented`, missing-but-optional,
//! and `Throttled` are advisory — they appear in the output but
//! do not flip the exit code.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use anyhow::{Context, Result, bail};
use clap::Args;
use devboy_core::{LivenessProbe, LivenessResult, liveness::LivenessStatus};
use devboy_secret_env_store::convention_name;
use devboy_secret_patterns::Catalogue;
use devboy_storage::{
    FormatCheck, GlobalIndex, ProjectManifest, ResolvedSecret, SecretPath, merge_manifest,
    validate_format,
};
use secrecy::SecretString;
use serde::Serialize;
use serde_json::json;

/// Flags for `devboy secrets validate`.
#[derive(Args, Debug, Default)]
pub struct ValidateArgs {
    /// Validate just one path. Defaults to all manifest paths.
    pub path: Option<String>,
    /// Format check only — skip the upstream liveness probe.
    /// This is the implied default; pass `--format-only` to be
    /// explicit (e.g. when `--liveness` is set in a CI variable
    /// you want to override locally).
    #[arg(long)]
    pub format_only: bool,
    /// Run upstream liveness probes (github, gitlab) in addition
    /// to the format check.
    #[arg(long)]
    pub liveness: bool,
    /// Print every path, including ones that pass cleanly. By
    /// default the command shows only failures, warnings, and
    /// skipped rows.
    #[arg(long)]
    pub all: bool,
    /// Print as JSON (one object per path) instead of a
    /// human-readable table.
    #[arg(long)]
    pub json: bool,
}

// =============================================================================
// Per-path row
// =============================================================================

#[derive(Debug, Serialize)]
struct ValidationRow {
    path: String,
    required: bool,
    format: FormatRowStatus,
    liveness: Option<LivenessRowStatus>,
    /// `Pass` / `Warning` / `Error` / `Skipped` — the per-row
    /// fold across format + liveness.
    overall: RowSeverity,
    /// Provider derived from the path's middle segment. Used to
    /// pick a `LivenessProbe`. `None` when the convention can't
    /// produce one (e.g. the path has fewer than three segments,
    /// which never happens for valid `SecretPath`s).
    provider: Option<String>,
    /// Convention-derived env var name we consulted for the
    /// candidate value.
    env_var: String,
    /// `true` when an env var with that name was set (and thus
    /// validatable). When `false`, the row is `MissingValue`.
    value_present: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
enum FormatRowStatus {
    /// Format passed.
    Pass {
        /// Where the regex came from.
        source: String,
    },
    /// Format mismatched — value was present but did not match
    /// the regex.
    Mismatch { expected: String },
    /// `format_regex` failed to compile / pattern_id missing /
    /// other validator error.
    Error { message: String },
    /// No rule (no `format_regex`, no `pattern_id`).
    NoRule,
    /// No value to validate (env var unset). Doesn't run the
    /// format check at all.
    MissingValue,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
enum LivenessRowStatus {
    Live {
        detail: Option<String>,
        expires_at: Option<String>,
    },
    Revoked {
        detail: Option<String>,
    },
    Expired {
        expires_at: Option<String>,
    },
    Throttled {
        detail: Option<String>,
    },
    NotImplemented {
        provider: String,
    },
    Error {
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum RowSeverity {
    Pass,
    Warning,
    Error,
    Skipped,
}

impl RowSeverity {
    fn rank(self) -> u8 {
        match self {
            Self::Skipped => 0,
            Self::Pass => 1,
            Self::Warning => 2,
            Self::Error => 3,
        }
    }
}

// =============================================================================
// Dispatch
// =============================================================================

pub async fn handle(args: ValidateArgs) -> Result<()> {
    let global = GlobalIndex::load().context("failed to load global index")?;
    let manifest = ProjectManifest::load().context("failed to load project manifest")?;
    let merged = merge_manifest(&global, &manifest).context("failed to merge manifest")?;
    let catalogue = Catalogue::builtins_only();

    let filter = args
        .path
        .as_deref()
        .map(SecretPath::parse)
        .transpose()
        .with_context(|| format!("invalid path '{:?}'", args.path))?;

    let mut rows = Vec::new();
    for resolved in merged.secrets.values() {
        if let Some(only) = &filter
            && resolved.path != *only
        {
            continue;
        }
        let row = build_row(resolved, &catalogue, args.liveness).await;
        rows.push(row);
    }

    if let Some(only) = &filter
        && rows.is_empty()
    {
        bail!("path '{}' is not declared in the active manifest", only);
    }

    let visible: Vec<&ValidationRow> = if args.all {
        rows.iter().collect()
    } else {
        rows.iter()
            .filter(|r| r.overall != RowSeverity::Pass)
            .collect()
    };

    if args.json {
        print_json(&visible);
    } else {
        print_human(&visible, rows.len());
    }

    let any_error = rows.iter().any(|r| r.overall == RowSeverity::Error);
    if any_error {
        bail!("validation failed: at least one required path did not validate");
    }
    Ok(())
}

// =============================================================================
// Row builder
// =============================================================================

async fn build_row(
    resolved: &ResolvedSecret,
    catalogue: &Catalogue,
    run_liveness: bool,
) -> ValidationRow {
    let provider = derive_provider(&resolved.path);
    let env_var = convention_name(resolved.path.as_str());
    let candidate = std::env::var(&env_var).ok().filter(|v| !v.is_empty());

    let format_status = match &candidate {
        None => FormatRowStatus::MissingValue,
        Some(v) => match validate_format(&resolved.metadata, v, catalogue) {
            FormatCheck::Ok { source } => FormatRowStatus::Pass {
                source: format!("{source:?}").to_lowercase(),
            },
            FormatCheck::Mismatch { expected, .. } => FormatRowStatus::Mismatch { expected },
            FormatCheck::Error { message } => FormatRowStatus::Error { message },
            FormatCheck::NoRule => FormatRowStatus::NoRule,
        },
    };

    let liveness_status = if run_liveness && candidate.is_some() {
        match probe_liveness(provider.as_deref(), candidate.as_deref().unwrap()).await {
            Some(result) => Some(map_liveness(provider.as_deref(), &result)),
            None => Some(LivenessRowStatus::NotImplemented {
                provider: provider.clone().unwrap_or_else(|| "unknown".to_owned()),
            }),
        }
    } else {
        None
    };

    let overall = fold_severity(resolved, &format_status, liveness_status.as_ref());

    ValidationRow {
        path: resolved.path.to_string(),
        required: resolved.required,
        format: format_status,
        liveness: liveness_status,
        overall,
        provider,
        env_var,
        value_present: candidate.is_some(),
    }
}

fn fold_severity(
    resolved: &ResolvedSecret,
    format: &FormatRowStatus,
    liveness: Option<&LivenessRowStatus>,
) -> RowSeverity {
    // Collect per-input contributions, then pick the worst.
    // Treating each check as an independent contribution lets a
    // row whose only signals are `Skipped`s end up `Skipped`
    // overall — instead of misleadingly reading as a clean Pass.
    let mut contributions = Vec::with_capacity(2);
    contributions.push(match format {
        FormatRowStatus::Pass { .. } => RowSeverity::Pass,
        FormatRowStatus::NoRule => RowSeverity::Skipped,
        FormatRowStatus::MissingValue => {
            if resolved.required {
                RowSeverity::Error
            } else {
                RowSeverity::Skipped
            }
        }
        FormatRowStatus::Mismatch { .. } | FormatRowStatus::Error { .. } => {
            if resolved.required {
                RowSeverity::Error
            } else {
                RowSeverity::Warning
            }
        }
    });
    if let Some(l) = liveness {
        contributions.push(match l {
            LivenessRowStatus::Live { .. } => RowSeverity::Pass,
            LivenessRowStatus::Revoked { .. } | LivenessRowStatus::Expired { .. } => {
                if resolved.required {
                    RowSeverity::Error
                } else {
                    RowSeverity::Warning
                }
            }
            LivenessRowStatus::Throttled { .. }
            | LivenessRowStatus::NotImplemented { .. }
            | LivenessRowStatus::Error { .. } => RowSeverity::Warning,
        });
    }
    contributions
        .into_iter()
        .max_by_key(|s| s.rank())
        .unwrap_or(RowSeverity::Skipped)
}

fn derive_provider(path: &SecretPath) -> Option<String> {
    let segments: Vec<&str> = path.as_str().split('/').collect();
    if segments.len() >= 3 {
        Some(segments[1].to_owned())
    } else {
        None
    }
}

async fn probe_liveness(provider: Option<&str>, value: &str) -> Option<LivenessResult> {
    let provider = provider?;
    let token = SecretString::from(value.to_owned());
    match provider {
        "github" => {
            let client = devboy_github::GitHubClient::new(
                "owner",
                "repo",
                SecretString::from(String::new()),
            );
            client.test(&token).await.ok()
        }
        "gitlab" => {
            let client = devboy_gitlab::GitLabClient::new("1", SecretString::from(String::new()));
            client.test(&token).await.ok()
        }
        _ => None,
    }
}

fn map_liveness(provider: Option<&str>, r: &LivenessResult) -> LivenessRowStatus {
    match r.status {
        LivenessStatus::Live => LivenessRowStatus::Live {
            detail: r.detail.clone(),
            expires_at: r.expires_at.clone(),
        },
        LivenessStatus::Revoked => LivenessRowStatus::Revoked {
            detail: r.detail.clone(),
        },
        LivenessStatus::Expired => LivenessRowStatus::Expired {
            expires_at: r.expires_at.clone(),
        },
        LivenessStatus::Throttled => LivenessRowStatus::Throttled {
            detail: r.detail.clone(),
        },
        LivenessStatus::NotImplemented => LivenessRowStatus::NotImplemented {
            provider: provider.unwrap_or("unknown").to_owned(),
        },
        LivenessStatus::Error => LivenessRowStatus::Error {
            detail: r.detail.clone(),
        },
    }
}

// =============================================================================
// Output
// =============================================================================

fn print_json(rows: &[&ValidationRow]) {
    let payload = json!({ "secrets": rows });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
}

fn print_human(rows: &[&ValidationRow], total: usize) {
    if rows.is_empty() {
        println!("validate: {total} secret(s); all pass");
        return;
    }
    let mut path_w = "PATH".len();
    let mut format_w = "FORMAT".len();
    let mut liveness_w = "LIVENESS".len();
    for r in rows {
        path_w = path_w.max(r.path.len());
        format_w = format_w.max(format_label(&r.format).len());
        liveness_w = liveness_w.max(liveness_label(r.liveness.as_ref()).len());
    }
    println!(
        "{:<width_path$}  {:<width_format$}  {:<width_liveness$}  STATUS",
        "PATH",
        "FORMAT",
        "LIVENESS",
        width_path = path_w,
        width_format = format_w,
        width_liveness = liveness_w,
    );
    for r in rows {
        println!(
            "{:<width_path$}  {:<width_format$}  {:<width_liveness$}  {}",
            r.path,
            format_label(&r.format),
            liveness_label(r.liveness.as_ref()),
            severity_label(r.overall),
            width_path = path_w,
            width_format = format_w,
            width_liveness = liveness_w,
        );
        match &r.format {
            FormatRowStatus::Mismatch { expected } => {
                println!("    expected format: {expected}");
            }
            FormatRowStatus::Error { message } => {
                println!("    format error: {message}");
            }
            FormatRowStatus::MissingValue if r.required => {
                println!("    set ${} or add it to the credential store", r.env_var);
            }
            _ => {}
        }
        if let Some(LivenessRowStatus::Revoked { detail })
        | Some(LivenessRowStatus::Error { detail })
        | Some(LivenessRowStatus::Throttled { detail }) = r.liveness.as_ref()
            && let Some(d) = detail
        {
            println!("    liveness: {d}");
        }
    }
    println!();
    let summary_total = total;
    let shown = rows.len();
    println!("{shown} of {summary_total} secret(s) shown — pass `--all` to include passing rows",);
}

fn format_label(s: &FormatRowStatus) -> String {
    match s {
        FormatRowStatus::Pass { source } => format!("ok ({source})"),
        FormatRowStatus::Mismatch { .. } => "mismatch".to_owned(),
        FormatRowStatus::Error { .. } => "error".to_owned(),
        FormatRowStatus::NoRule => "no-rule".to_owned(),
        FormatRowStatus::MissingValue => "no-value".to_owned(),
    }
}

fn liveness_label(s: Option<&LivenessRowStatus>) -> String {
    match s {
        None => "—".to_owned(),
        Some(LivenessRowStatus::Live { .. }) => "live".to_owned(),
        Some(LivenessRowStatus::Revoked { .. }) => "revoked".to_owned(),
        Some(LivenessRowStatus::Expired { .. }) => "expired".to_owned(),
        Some(LivenessRowStatus::Throttled { .. }) => "throttled".to_owned(),
        Some(LivenessRowStatus::NotImplemented { .. }) => "not-impl".to_owned(),
        Some(LivenessRowStatus::Error { .. }) => "error".to_owned(),
    }
}

fn severity_label(s: RowSeverity) -> &'static str {
    match s {
        RowSeverity::Pass => "pass",
        RowSeverity::Warning => "warn",
        RowSeverity::Error => "FAIL",
        RowSeverity::Skipped => "skip",
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_storage::{IndexEntry, SecretOrigin};

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    fn resolved(path: &str, required: bool, entry: IndexEntry) -> ResolvedSecret {
        ResolvedSecret {
            path: p(path),
            required,
            origin: SecretOrigin::ProjectLocal,
            metadata: entry,
        }
    }

    // -- derive_provider ------------------------------------------

    #[test]
    fn derive_provider_uses_middle_segment() {
        assert_eq!(
            derive_provider(&p("team/github/token-deploy")),
            Some("github".into())
        );
        assert_eq!(
            derive_provider(&p("personal/gitlab/pat")),
            Some("gitlab".into())
        );
    }

    // -- fold_severity --------------------------------------------

    #[test]
    fn required_missing_value_is_error() {
        let r = resolved("a/b/c", true, IndexEntry::default());
        let s = fold_severity(&r, &FormatRowStatus::MissingValue, None);
        assert_eq!(s, RowSeverity::Error);
    }

    #[test]
    fn optional_missing_value_is_skipped() {
        let r = resolved("a/b/c", false, IndexEntry::default());
        let s = fold_severity(&r, &FormatRowStatus::MissingValue, None);
        assert_eq!(s, RowSeverity::Skipped);
    }

    #[test]
    fn required_format_mismatch_is_error() {
        let r = resolved("a/b/c", true, IndexEntry::default());
        let s = fold_severity(
            &r,
            &FormatRowStatus::Mismatch {
                expected: "^x".into(),
            },
            None,
        );
        assert_eq!(s, RowSeverity::Error);
    }

    #[test]
    fn optional_format_mismatch_is_warning() {
        let r = resolved("a/b/c", false, IndexEntry::default());
        let s = fold_severity(
            &r,
            &FormatRowStatus::Mismatch {
                expected: "^x".into(),
            },
            None,
        );
        assert_eq!(s, RowSeverity::Warning);
    }

    #[test]
    fn liveness_revoked_on_required_is_error_even_if_format_passed() {
        let r = resolved("a/b/c", true, IndexEntry::default());
        let s = fold_severity(
            &r,
            &FormatRowStatus::Pass {
                source: "inline".into(),
            },
            Some(&LivenessRowStatus::Revoked {
                detail: Some("revoked".into()),
            }),
        );
        assert_eq!(s, RowSeverity::Error);
    }

    #[test]
    fn liveness_throttled_is_just_warning() {
        let r = resolved("a/b/c", true, IndexEntry::default());
        let s = fold_severity(
            &r,
            &FormatRowStatus::Pass {
                source: "inline".into(),
            },
            Some(&LivenessRowStatus::Throttled {
                detail: Some("retry-after: 60".into()),
            }),
        );
        assert_eq!(s, RowSeverity::Warning);
    }

    #[test]
    fn no_rule_with_value_falls_through_to_skipped() {
        // Path with no format_regex / pattern_id — silent.
        let r = resolved("a/b/c", true, IndexEntry::default());
        let s = fold_severity(&r, &FormatRowStatus::NoRule, None);
        assert_eq!(s, RowSeverity::Skipped);
    }

    #[test]
    fn pass_format_with_no_liveness_is_pass() {
        let r = resolved("a/b/c", true, IndexEntry::default());
        let s = fold_severity(
            &r,
            &FormatRowStatus::Pass {
                source: "inline".into(),
            },
            None,
        );
        assert_eq!(s, RowSeverity::Pass);
    }

    // -- rank ordering --------------------------------------------

    #[test]
    fn rank_orders_skipped_below_pass_below_warning_below_error() {
        // Document the rank ordering tests rely on; if this
        // changes, fold_severity's max_by_key behaviour changes
        // with it.
        assert!(RowSeverity::Skipped.rank() < RowSeverity::Pass.rank());
        assert!(RowSeverity::Pass.rank() < RowSeverity::Warning.rank());
        assert!(RowSeverity::Warning.rank() < RowSeverity::Error.rank());
    }

    // -- Labels ----------------------------------------------------

    #[test]
    fn format_label_handles_each_status() {
        assert_eq!(
            format_label(&FormatRowStatus::Pass {
                source: "inline".into()
            }),
            "ok (inline)"
        );
        assert_eq!(
            format_label(&FormatRowStatus::Mismatch {
                expected: "x".into()
            }),
            "mismatch"
        );
        assert_eq!(
            format_label(&FormatRowStatus::Error {
                message: "x".into()
            }),
            "error"
        );
        assert_eq!(format_label(&FormatRowStatus::NoRule), "no-rule");
        assert_eq!(format_label(&FormatRowStatus::MissingValue), "no-value");
    }

    #[test]
    fn liveness_label_handles_each_status() {
        assert_eq!(liveness_label(None), "—");
        assert_eq!(
            liveness_label(Some(&LivenessRowStatus::Live {
                detail: None,
                expires_at: None
            })),
            "live"
        );
        assert_eq!(
            liveness_label(Some(&LivenessRowStatus::Revoked { detail: None })),
            "revoked"
        );
        assert_eq!(
            liveness_label(Some(&LivenessRowStatus::Expired { expires_at: None })),
            "expired"
        );
    }

    // ===========================================================
    // T3 — labels for the remaining variants, map_liveness, more
    //      fold_severity branches, build_row happy + edge cases
    // ===========================================================

    #[test]
    fn liveness_label_covers_throttled_not_implemented_and_error() {
        // The existing test only exercised the first three
        // variants; pin the rest so a future rename of the
        // user-facing strings (which docs / scripts grep for)
        // shows up as a test failure.
        assert_eq!(
            liveness_label(Some(&LivenessRowStatus::Throttled { detail: None })),
            "throttled"
        );
        assert_eq!(
            liveness_label(Some(&LivenessRowStatus::NotImplemented {
                provider: "exotic".into()
            })),
            "not-impl"
        );
        assert_eq!(
            liveness_label(Some(&LivenessRowStatus::Error { detail: None })),
            "error"
        );
    }

    #[test]
    fn severity_label_covers_all_four_variants() {
        assert_eq!(severity_label(RowSeverity::Pass), "pass");
        assert_eq!(severity_label(RowSeverity::Warning), "warn");
        assert_eq!(severity_label(RowSeverity::Error), "FAIL");
        assert_eq!(severity_label(RowSeverity::Skipped), "skip");
    }

    // -- map_liveness ---------------------------------------------

    #[test]
    fn map_liveness_live_preserves_detail_and_expires_at() {
        let r = LivenessResult {
            status: LivenessStatus::Live,
            detail: Some("alice/repo".into()),
            expires_at: Some("2026-12-31".into()),
        };
        match map_liveness(Some("github"), &r) {
            LivenessRowStatus::Live { detail, expires_at } => {
                assert_eq!(detail.as_deref(), Some("alice/repo"));
                assert_eq!(expires_at.as_deref(), Some("2026-12-31"));
            }
            other => panic!("expected Live row status, got {other:?}"),
        }
    }

    #[test]
    fn map_liveness_revoked_keeps_only_detail() {
        let r = LivenessResult::revoked("token has been revoked");
        match map_liveness(Some("gitlab"), &r) {
            LivenessRowStatus::Revoked { detail } => {
                assert_eq!(detail.as_deref(), Some("token has been revoked"));
            }
            other => panic!("expected Revoked row status, got {other:?}"),
        }
    }

    #[test]
    fn map_liveness_expired_keeps_only_expires_at() {
        let r = LivenessResult::expired("2026-01-01");
        match map_liveness(Some("github"), &r) {
            LivenessRowStatus::Expired { expires_at } => {
                assert_eq!(expires_at.as_deref(), Some("2026-01-01"));
            }
            other => panic!("expected Expired row status, got {other:?}"),
        }
    }

    #[test]
    fn map_liveness_throttled_keeps_detail() {
        let r = LivenessResult::throttled("Retry-After: 60");
        match map_liveness(Some("gitlab"), &r) {
            LivenessRowStatus::Throttled { detail } => {
                assert_eq!(detail.as_deref(), Some("Retry-After: 60"));
            }
            other => panic!("expected Throttled row status, got {other:?}"),
        }
    }

    #[test]
    fn map_liveness_not_implemented_uses_provider_arg_not_result_detail() {
        // The mapper's `provider` arg wins over whatever the
        // LivenessResult convenience filled in — this is the
        // ADR-021 §6 contract: `NotImplemented` rows surface
        // the provider name the manifest derived, not the
        // probe's own self-description.
        let r = LivenessResult::not_implemented("self-named");
        match map_liveness(Some("slack"), &r) {
            LivenessRowStatus::NotImplemented { provider } => {
                assert_eq!(provider, "slack");
            }
            other => panic!("expected NotImplemented row status, got {other:?}"),
        }
    }

    #[test]
    fn map_liveness_not_implemented_falls_back_to_unknown_when_provider_is_none() {
        let r = LivenessResult::not_implemented("any");
        match map_liveness(None, &r) {
            LivenessRowStatus::NotImplemented { provider } => {
                assert_eq!(provider, "unknown");
            }
            other => panic!("expected NotImplemented row status, got {other:?}"),
        }
    }

    #[test]
    fn map_liveness_error_keeps_detail() {
        let r = LivenessResult::error("connection reset");
        match map_liveness(Some("github"), &r) {
            LivenessRowStatus::Error { detail } => {
                assert_eq!(detail.as_deref(), Some("connection reset"));
            }
            other => panic!("expected Error row status, got {other:?}"),
        }
    }

    // -- fold_severity, remaining branches -----------------------

    #[test]
    fn pass_format_with_live_liveness_is_pass() {
        let r = resolved("a/b/c", true, IndexEntry::default());
        let s = fold_severity(
            &r,
            &FormatRowStatus::Pass {
                source: "inline".into(),
            },
            Some(&LivenessRowStatus::Live {
                detail: None,
                expires_at: None,
            }),
        );
        assert_eq!(s, RowSeverity::Pass);
    }

    #[test]
    fn optional_expired_liveness_is_warning() {
        // Required-expired is already covered by the existing
        // `liveness_revoked_on_required_is_error_even_if_format_passed`
        // test (Revoked and Expired share a branch in
        // fold_severity). This pins the OPTIONAL side of that
        // branch — same severity rule but a warn outcome.
        let r = resolved("a/b/c", false, IndexEntry::default());
        let s = fold_severity(
            &r,
            &FormatRowStatus::Pass {
                source: "inline".into(),
            },
            Some(&LivenessRowStatus::Expired {
                expires_at: Some("2026-01-01".into()),
            }),
        );
        assert_eq!(s, RowSeverity::Warning);
    }

    #[test]
    fn not_implemented_liveness_is_warning_regardless_of_required_flag() {
        // NotImplemented is a "we couldn't tell" signal. Both
        // required and optional rows surface it as a warning,
        // never as an error, so unsupported providers don't
        // gate CI by accident.
        for required in [true, false] {
            let r = resolved("a/b/c", required, IndexEntry::default());
            let s = fold_severity(
                &r,
                &FormatRowStatus::Pass {
                    source: "inline".into(),
                },
                Some(&LivenessRowStatus::NotImplemented {
                    provider: "exotic".into(),
                }),
            );
            assert_eq!(
                s,
                RowSeverity::Warning,
                "NotImplemented liveness with required={required} must be Warning",
            );
        }
    }

    #[test]
    fn error_liveness_is_warning_regardless_of_required_flag() {
        // Mirror of `not_implemented_liveness_is_warning_*`:
        // a transient probe failure must not gate CI.
        for required in [true, false] {
            let r = resolved("a/b/c", required, IndexEntry::default());
            let s = fold_severity(
                &r,
                &FormatRowStatus::Pass {
                    source: "inline".into(),
                },
                Some(&LivenessRowStatus::Error {
                    detail: Some("dns failed".into()),
                }),
            );
            assert_eq!(
                s,
                RowSeverity::Warning,
                "Error liveness with required={required} must be Warning",
            );
        }
    }

    // -- build_row ------------------------------------------------

    /// Build a runtime that drives one `build_row` call so the
    /// async surface gets coverage without spinning up a full
    /// `#[tokio::test]` everywhere.
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn build_row_missing_required_value_renders_as_error() {
        // No env var set → MissingValue + required ⇒ Error.
        // Use a uniquely-named path so concurrent tests can't
        // collide on the convention env var.
        let path = "team/buildrow/missing-req-1";
        let env = convention_name(path);
        // Sanity: the env var must not be set in the host env.
        // Tests run in arbitrary order so we cannot trust the
        // global env here — if some other test set it for any
        // reason, fail loudly rather than silently passing the
        // wrong branch.
        assert!(
            std::env::var(&env).is_err(),
            "test pollution: {env} is set in the host env"
        );

        let r = resolved(path, true, IndexEntry::default());
        let catalogue = Catalogue::builtins_only();
        let row = block_on(build_row(&r, &catalogue, false));
        assert_eq!(row.overall, RowSeverity::Error);
        assert!(matches!(row.format, FormatRowStatus::MissingValue));
        assert!(row.liveness.is_none(), "no liveness without --liveness");
        assert!(!row.value_present);
        assert_eq!(row.env_var, env);
        assert_eq!(row.provider.as_deref(), Some("buildrow"));
    }

    #[test]
    fn build_row_optional_missing_value_renders_as_skipped() {
        let path = "team/buildrow/missing-opt-2";
        let env = convention_name(path);
        assert!(std::env::var(&env).is_err());
        let r = resolved(path, false, IndexEntry::default());
        let catalogue = Catalogue::builtins_only();
        let row = block_on(build_row(&r, &catalogue, false));
        assert_eq!(row.overall, RowSeverity::Skipped);
        assert!(matches!(row.format, FormatRowStatus::MissingValue));
    }

    #[test]
    fn build_row_format_pass_when_value_matches_inline_regex() {
        // Path-specific env var lets this test set / unset
        // safely under temp_env without colliding with siblings.
        let path = "team/buildrow/format-pass-3";
        let env = convention_name(path);
        let entry = IndexEntry {
            format_regex: Some("^glpat-[a-z0-9]{6}$".into()),
            ..IndexEntry::default()
        };
        let r = resolved(path, true, entry);
        let catalogue = Catalogue::builtins_only();

        temp_env::with_var(&env, Some("glpat-abc123"), || {
            let row = block_on(build_row(&r, &catalogue, false));
            assert_eq!(row.overall, RowSeverity::Pass);
            match &row.format {
                FormatRowStatus::Pass { source } => {
                    // The Display of FormatCheckSource::Inline
                    // gets lowercased on the way out; just
                    // assert it's non-empty.
                    assert!(!source.is_empty(), "format source must be set");
                }
                other => panic!("expected Pass, got {other:?}"),
            }
            assert!(row.value_present);
        });
    }

    #[test]
    fn build_row_format_mismatch_renders_as_warning_when_optional() {
        // Mismatch + optional ⇒ Warning (not Error). Pins the
        // optional-relaxation branch.
        let path = "team/buildrow/format-mismatch-4";
        let env = convention_name(path);
        let entry = IndexEntry {
            format_regex: Some("^glpat-[a-z0-9]{6}$".into()),
            ..IndexEntry::default()
        };
        let r = resolved(path, false, entry);
        let catalogue = Catalogue::builtins_only();

        temp_env::with_var(&env, Some("wrong-shape"), || {
            let row = block_on(build_row(&r, &catalogue, false));
            assert_eq!(row.overall, RowSeverity::Warning);
            match &row.format {
                FormatRowStatus::Mismatch { expected } => {
                    assert_eq!(expected, "^glpat-[a-z0-9]{6}$");
                }
                other => panic!("expected Mismatch, got {other:?}"),
            }
        });
    }

    #[test]
    fn build_row_no_rule_keeps_required_row_at_skipped() {
        // No format_regex / pattern_id but a value is present
        // → NoRule format status, and overall Skipped because
        // there's nothing to assert against.
        let path = "team/buildrow/no-rule-5";
        let env = convention_name(path);
        let r = resolved(path, true, IndexEntry::default());
        let catalogue = Catalogue::builtins_only();

        temp_env::with_var(&env, Some("any-value-survives"), || {
            let row = block_on(build_row(&r, &catalogue, false));
            assert_eq!(row.overall, RowSeverity::Skipped);
            assert!(matches!(row.format, FormatRowStatus::NoRule));
            assert!(row.value_present);
        });
    }

    #[test]
    fn build_row_empty_env_var_is_treated_as_unset() {
        // Convention: an empty-string env var is *as if* the
        // var wasn't set at all (see `build_row`'s
        // `.filter(|v| !v.is_empty())`). This pins that
        // behaviour so CI containers that pre-export every
        // expected variable to "" don't accidentally validate.
        let path = "team/buildrow/empty-env-6";
        let env = convention_name(path);
        let r = resolved(path, true, IndexEntry::default());
        let catalogue = Catalogue::builtins_only();

        temp_env::with_var(&env, Some(""), || {
            let row = block_on(build_row(&r, &catalogue, false));
            assert!(matches!(row.format, FormatRowStatus::MissingValue));
            assert_eq!(row.overall, RowSeverity::Error);
            assert!(!row.value_present);
        });
    }
}
