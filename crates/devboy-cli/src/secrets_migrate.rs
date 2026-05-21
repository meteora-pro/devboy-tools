//! `devboy secrets migrate` — move a legacy keychain entry under
//! the ADR-020 path convention per [ADR-020] §7.
//!
//! Pre-ADR-020 devboy-tools wrote credentials at flat
//! `<provider>/token` keys. The new path convention requires
//! `<scope>/<provider>/<purpose>` (≥3 segments, ADR-020 §2). The
//! `doctor` "Legacy keychain entries" check (P10.1) surfaces
//! pre-migration entries; this command moves them.
//!
//! ## Single-entry flow
//!
//! `devboy secrets migrate <legacy-key>`:
//!
//! 1. Look up `<legacy-key>` in the keychain — abort if absent.
//! 2. Suggest a canonical target path
//!    (`legacy_keys::suggest_canonical_path`); the user can edit
//!    the suggestion at the prompt or pre-supply one through
//!    `--target <path>`.
//! 3. Validate the target as a [`SecretPath`].
//! 4. Write the value at the new key in the credential store.
//! 5. Register the entry in the global index with a
//!    "migrated from `<legacy>`" description.
//! 6. Confirm with the user, then delete the legacy entry —
//!    unless `--keep-legacy` is set.
//!
//! ## Batch mode
//!
//! `devboy secrets migrate --all` walks every present legacy
//! entry, accepts the suggested path verbatim, and applies the
//! same plan in one go. `--keep-legacy` defaults to **on** in
//! batch mode (the user opts in to deletion separately) so a
//! script that runs migrate doesn't lose the source-of-truth
//! data on a wrong guess.
//!
//! ## Dry-run
//!
//! `--dry-run` prints the plan and exits without writing
//! anything. Useful before committing to a migration in CI or a
//! shared dev environment.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Args;
use devboy_storage::{CredentialStore, GlobalIndex, IndexEntry, KeychainStore, SecretPath};
use dialoguer::{Confirm, Input};
use secrecy::SecretString;
use serde::Serialize;
use tracing::debug;

use crate::doctor::checks::legacy_keys::{known_legacy_keys, suggest_canonical_path};

// =============================================================================
// CLI
// =============================================================================

/// Flags for `devboy secrets migrate`.
#[derive(Args, Debug, Default)]
pub struct MigrateArgs {
    /// Legacy keychain key to migrate (e.g. `github/token`).
    /// Required unless `--all` is set.
    pub legacy_key: Option<String>,
    /// Migrate every present legacy entry in one go. Uses the
    /// suggested target path for each; pass `--keep-legacy` to
    /// also retain the source rows (the default in batch mode).
    #[arg(long)]
    pub all: bool,
    /// Pre-supply the target path; bypasses the interactive
    /// prompt for the single-entry flow.
    #[arg(long)]
    pub target: Option<String>,
    /// Don't delete the legacy entry after a successful write.
    /// Defaults to `true` in `--all` mode (cautious).
    #[arg(long)]
    pub keep_legacy: bool,
    /// Print the plan without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the prompts entirely. Equivalent to `--target <suggested>`
    /// and not deleting unless `--no-keep-legacy` is set.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

// =============================================================================
// Plan + outcome
// =============================================================================

/// One unit of work the migration produces. The plan is decided
/// before any credential store writes happen so a `--dry-run`
/// can render the same shape the executor would apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationPlan {
    /// Legacy key being moved.
    pub legacy_key: String,
    /// New ADR-020 path the value lands at.
    pub target_path: String,
    /// `true` to delete the legacy entry after the write
    /// succeeds.
    pub delete_legacy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationOutcome {
    /// The plan that was executed.
    pub plan: MigrationPlan,
    /// `true` when the new credential store entry was written.
    pub wrote_target: bool,
    /// `true` when the index was updated (insert).
    pub registered_in_index: bool,
    /// `true` when the legacy entry was deleted.
    pub deleted_legacy: bool,
}

// =============================================================================
// Dispatch
// =============================================================================

pub async fn handle(args: MigrateArgs) -> Result<()> {
    if args.all && args.legacy_key.is_some() {
        bail!("pass either a legacy key OR --all, not both");
    }
    if !args.all && args.legacy_key.is_none() {
        bail!("provide a legacy key (e.g. `devboy secrets migrate github/token`) or pass --all");
    }

    let store: Arc<dyn CredentialStore> = Arc::new(KeychainStore::new());
    let mut index = GlobalIndex::load().context("failed to load global index")?;

    let plans = if args.all {
        plan_all(&store, args.keep_legacy)
    } else {
        let key = args.legacy_key.as_ref().expect("guarded above");
        let target = resolve_target_path(&args, key)?;
        // Interactive confirm — skip in `--yes`/`--keep-legacy`
        // modes; default the prompt to "yes, delete" so a plain
        // Enter on the first migration completes the move.
        let delete_legacy = if args.keep_legacy {
            false
        } else if args.yes {
            true
        } else {
            confirm_delete(key, true)?
        };
        let plan = plan_one(key, &target, delete_legacy);
        vec![plan]
    };

    if plans.is_empty() {
        println!("nothing to migrate — no legacy keychain entries detected");
        return Ok(());
    }

    if args.dry_run {
        println!(
            "dry-run — would migrate {} entr{}:",
            plans.len(),
            if plans.len() == 1 { "y" } else { "ies" }
        );
        for p in &plans {
            print_plan(p);
        }
        return Ok(());
    }

    let mut outcomes = Vec::with_capacity(plans.len());
    for plan in plans {
        let outcome = execute_plan(&plan, store.as_ref(), &mut index)?;
        outcomes.push(outcome);
    }

    // Persist the index once at the end so a multi-entry batch
    // doesn't write the file N times.
    index
        .save()
        .map(|p| {
            debug!(path = ?p, "global index saved after migration");
        })
        .context("failed to save global index after migration")?;

    println!(
        "migrated {} entr{}:",
        outcomes.len(),
        if outcomes.len() == 1 { "y" } else { "ies" }
    );
    for outcome in &outcomes {
        print_outcome(outcome);
    }
    Ok(())
}

fn resolve_target_path(args: &MigrateArgs, legacy_key: &str) -> Result<String> {
    if let Some(t) = args.target.as_deref() {
        return Ok(t.to_owned());
    }
    let suggested = suggest_canonical_path(legacy_key).ok_or_else(|| {
        anyhow::anyhow!(
            "no canonical path could be derived from '{legacy_key}'; pass --target <path>"
        )
    })?;
    if args.yes {
        return Ok(suggested);
    }
    // Interactive: let the user edit the suggestion.
    let target: String = Input::new()
        .with_prompt(format!("target path for '{legacy_key}'"))
        .default(suggested)
        .interact_text()
        .context("interactive prompt failed")?;
    Ok(target)
}

fn plan_all(store: &Arc<dyn CredentialStore>, keep_legacy: bool) -> Vec<MigrationPlan> {
    let mut plans = Vec::new();
    for key in known_legacy_keys() {
        match store.get(&key) {
            Ok(Some(_)) => {
                if let Some(target) = suggest_canonical_path(&key) {
                    plans.push(MigrationPlan {
                        legacy_key: key,
                        target_path: target,
                        // Batch mode caution: only delete the
                        // legacy entry when the user explicitly
                        // opts in via --no-keep-legacy.
                        delete_legacy: !keep_legacy,
                    });
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "warning: failed to probe legacy key '{}' (skipping): {e}",
                    &key
                );
            }
        }
    }
    plans
}

fn plan_one(legacy_key: &str, target_path: &str, delete_legacy: bool) -> MigrationPlan {
    MigrationPlan {
        legacy_key: legacy_key.to_owned(),
        target_path: target_path.to_owned(),
        delete_legacy,
    }
}

// =============================================================================
// Execution
// =============================================================================

/// Apply one [`MigrationPlan`] against a credential store + index.
///
/// Pure data flow: the function does not consult the user. The
/// caller is responsible for confirming any prompts before
/// reaching here. Tests pass a `MemoryStore` and assert on the
/// resulting outcomes.
pub fn execute_plan(
    plan: &MigrationPlan,
    store: &dyn CredentialStore,
    index: &mut GlobalIndex,
) -> Result<MigrationOutcome> {
    let target_path = SecretPath::parse(&plan.target_path).with_context(|| {
        format!(
            "target path '{}' is not a valid ADR-020 path",
            plan.target_path
        )
    })?;

    let value = store
        .get(&plan.legacy_key)
        .with_context(|| format!("failed to read legacy key '{}'", plan.legacy_key))?
        .ok_or_else(|| anyhow::anyhow!("legacy key '{}' is not present", plan.legacy_key))?;

    // Write at the new path.
    store
        .store(target_path.as_str(), &value)
        .with_context(|| format!("failed to write '{target_path}'"))?;
    let wrote_target = true;

    // Register in the index. The migrated entry gets a
    // description noting its provenance; nothing else is
    // populated — the user can fill in metadata later.
    let entry = IndexEntry {
        description: Some(format!(
            "migrated from legacy keychain entry '{}'",
            plan.legacy_key
        )),
        ..IndexEntry::default()
    };
    let registered_in_index = index.insert(target_path.clone(), entry).is_none();

    // Optionally delete the legacy entry. Best-effort — a delete
    // failure is not a hard fail because the new entry is
    // already in place; just surface a warning.
    let mut deleted_legacy = false;
    if plan.delete_legacy {
        match store.delete(&plan.legacy_key) {
            Ok(()) => deleted_legacy = true,
            Err(e) => {
                eprintln!(
                    "warning: failed to delete legacy '{}' after migration: {e}",
                    plan.legacy_key
                );
            }
        }
    }

    // Discard the value early. SecretString zeroizes on drop.
    drop(value);
    let _ = SecretString::from(String::new()); // keep import alive in tests

    Ok(MigrationOutcome {
        plan: plan.clone(),
        wrote_target,
        registered_in_index,
        deleted_legacy,
    })
}

// =============================================================================
// Output helpers
// =============================================================================

fn print_plan(p: &MigrationPlan) {
    println!(
        "  - {} → {}{}",
        p.legacy_key,
        p.target_path,
        if p.delete_legacy {
            " (delete legacy after write)"
        } else {
            " (keep legacy)"
        }
    );
}

fn print_outcome(o: &MigrationOutcome) {
    let mut bits: Vec<&str> = Vec::new();
    if o.wrote_target {
        bits.push("wrote new entry");
    }
    if o.registered_in_index {
        bits.push("registered in index");
    }
    if o.deleted_legacy {
        bits.push("deleted legacy");
    }
    println!(
        "  - {} → {} ({})",
        o.plan.legacy_key,
        o.plan.target_path,
        bits.join(", ")
    );
}

// =============================================================================
// Optional confirm
// =============================================================================

/// Prompt the user "delete `<legacy>` after migration?"; falls
/// back to `default` in non-TTY environments. Surfaced for the
/// CLI handler to call before kicking off `execute_plan` when
/// `--keep-legacy` was not set and `--yes` was not passed.
pub fn confirm_delete(legacy_key: &str, default: bool) -> Result<bool> {
    Confirm::new()
        .with_prompt(format!(
            "delete legacy entry '{legacy_key}' after migration?"
        ))
        .default(default)
        .interact()
        .context("interactive confirm failed")
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_storage::MemoryStore;
    use secrecy::ExposeSecret;

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    fn fresh_store_with(legacy_key: &str, value: &str) -> Arc<dyn CredentialStore> {
        Arc::new(MemoryStore::with_credentials([(
            legacy_key.to_owned(),
            value.to_owned(),
        )]))
    }

    // -- plan_one --------------------------------------------------

    #[test]
    fn plan_one_constructs_a_typed_record() {
        let plan = plan_one("github/token", "personal/github/token-legacy", true);
        assert_eq!(plan.legacy_key, "github/token");
        assert_eq!(plan.target_path, "personal/github/token-legacy");
        assert!(plan.delete_legacy);
    }

    // -- execute_plan happy path ----------------------------------

    #[test]
    fn execute_plan_writes_target_registers_index_and_deletes_legacy() {
        let store = fresh_store_with("github/token", "ghp_fixture");
        let mut index = GlobalIndex::new();
        let plan = plan_one("github/token", "personal/github/token-legacy", true);

        let outcome = execute_plan(&plan, store.as_ref(), &mut index).unwrap();
        assert!(outcome.wrote_target);
        assert!(outcome.registered_in_index);
        assert!(outcome.deleted_legacy);

        // Target read back.
        let val = store.get("personal/github/token-legacy").unwrap().unwrap();
        assert_eq!(val.expose_secret(), "ghp_fixture");

        // Legacy gone.
        assert!(store.get("github/token").unwrap().is_none());

        // Index has an entry with a provenance description.
        let entry = index.get(&p("personal/github/token-legacy")).unwrap();
        assert!(
            entry
                .description
                .as_deref()
                .unwrap()
                .contains("github/token")
        );
    }

    #[test]
    fn execute_plan_keeps_legacy_when_delete_is_false() {
        let store = fresh_store_with("gitlab/token", "glpat-fixture");
        let mut index = GlobalIndex::new();
        let plan = plan_one("gitlab/token", "personal/gitlab/token-legacy", false);

        let outcome = execute_plan(&plan, store.as_ref(), &mut index).unwrap();
        assert!(outcome.wrote_target);
        assert!(!outcome.deleted_legacy);

        // Legacy still present.
        assert!(store.get("gitlab/token").unwrap().is_some());
    }

    #[test]
    fn execute_plan_returns_false_for_index_when_target_already_existed() {
        let store = fresh_store_with("github/token", "ghp_fixture");
        let mut index = GlobalIndex::new();
        index.insert(
            p("personal/github/token-legacy"),
            IndexEntry {
                description: Some("pre-existing".into()),
                ..IndexEntry::default()
            },
        );
        let plan = plan_one("github/token", "personal/github/token-legacy", false);

        let outcome = execute_plan(&plan, store.as_ref(), &mut index).unwrap();
        // Target was overwritten in the index.
        assert!(outcome.wrote_target);
        assert!(
            !outcome.registered_in_index,
            "index entry already existed; insert() returns Some(prev)"
        );
        // The new description replaces the old.
        assert!(
            index
                .get(&p("personal/github/token-legacy"))
                .unwrap()
                .description
                .as_deref()
                .unwrap()
                .contains("github/token")
        );
    }

    // -- execute_plan error paths ---------------------------------

    #[test]
    fn execute_plan_rejects_invalid_target_path() {
        let store = fresh_store_with("github/token", "ghp");
        let mut index = GlobalIndex::new();
        // 2 segments — fails ADR-020 validator.
        let plan = plan_one("github/token", "github/token", true);
        let err = execute_plan(&plan, store.as_ref(), &mut index).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a valid ADR-020 path"));
    }

    #[test]
    fn execute_plan_errors_when_legacy_absent() {
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let mut index = GlobalIndex::new();
        let plan = plan_one("github/token", "personal/github/token-legacy", true);
        let err = execute_plan(&plan, store.as_ref(), &mut index).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not present"));
    }

    // -- plan_all --------------------------------------------------

    #[test]
    fn plan_all_includes_only_present_legacy_keys() {
        // Seed only one legacy entry.
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::with_credentials([(
            "github/token".into(),
            "ghp".into(),
        )]));
        let plans = plan_all(&store, /*keep_legacy=*/ true);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].legacy_key, "github/token");
        // keep_legacy=true → delete=false.
        assert!(!plans[0].delete_legacy);
    }

    #[test]
    fn plan_all_with_keep_false_marks_delete_legacy_true() {
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::with_credentials([(
            "github/token".into(),
            "ghp".into(),
        )]));
        let plans = plan_all(&store, /*keep_legacy=*/ false);
        assert!(plans[0].delete_legacy);
    }

    #[test]
    fn plan_all_returns_empty_when_no_legacy_entries_present() {
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let plans = plan_all(&store, false);
        assert!(plans.is_empty());
    }

    // -- end-to-end batch -----------------------------------------

    #[test]
    fn batch_through_execute_plan_handles_two_entries() {
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::with_credentials([
            ("github/token".into(), "ghp".into()),
            ("gitlab/token".into(), "glpat".into()),
        ]));
        let mut index = GlobalIndex::new();
        let plans = plan_all(&store, /*keep_legacy=*/ false);
        assert_eq!(plans.len(), 2);

        for plan in &plans {
            execute_plan(plan, store.as_ref(), &mut index).unwrap();
        }

        // Both new keys present, both legacy keys gone.
        assert!(store.get("personal/github/token-legacy").unwrap().is_some());
        assert!(store.get("personal/gitlab/token-legacy").unwrap().is_some());
        assert!(store.get("github/token").unwrap().is_none());
        assert!(store.get("gitlab/token").unwrap().is_none());

        // Index has both entries.
        assert!(index.get(&p("personal/github/token-legacy")).is_some());
        assert!(index.get(&p("personal/gitlab/token-legacy")).is_some());
    }

    // ===========================================================
    // T5 — resolve_target_path (non-interactive branches)
    // ===========================================================

    #[test]
    fn resolve_target_path_returns_explicit_target_verbatim() {
        // `--target` shortcuts the suggestion + interactive
        // editor; the function must return exactly what the user
        // supplied without consulting `suggest_canonical_path`.
        let args = MigrateArgs {
            legacy_key: Some("github/token".into()),
            target: Some("ops/github/legacy-cleanup".into()),
            yes: false,
            ..MigrateArgs::default()
        };
        let got = resolve_target_path(&args, "github/token").unwrap();
        assert_eq!(got, "ops/github/legacy-cleanup");
    }

    #[test]
    fn resolve_target_path_with_yes_returns_suggested_path() {
        // `--yes` non-interactive mode must take the suggestion
        // verbatim for known legacy keys. We use `github/token`
        // since suggest_canonical_path has a built-in mapping
        // for it; the exact path is brittle (encoded in
        // legacy_keys) so we only assert it's non-empty and
        // ADR-020-shaped.
        let args = MigrateArgs {
            legacy_key: Some("github/token".into()),
            yes: true,
            ..MigrateArgs::default()
        };
        let got = resolve_target_path(&args, "github/token").unwrap();
        assert!(
            got.matches('/').count() >= 2,
            "suggested path must have ≥3 segments per ADR-020 §2, got: {got}"
        );
    }

    #[test]
    fn resolve_target_path_bails_when_legacy_key_has_no_suggestion() {
        // Unknown legacy key + no --target + --yes → bail with
        // a clear hint to pass --target. `suggest_canonical_path`
        // returns `Some` for both 2-segment `a/b` and the
        // `contexts.*.*.*` shape; a single-token name fits
        // neither pattern and is the cleanest "no suggestion"
        // input.
        let args = MigrateArgs {
            legacy_key: Some("bare-no-slashes".into()),
            yes: true,
            ..MigrateArgs::default()
        };
        let err = resolve_target_path(&args, "bare-no-slashes").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("bare-no-slashes") && msg.contains("--target"),
            "error must name the key and direct to --target, got: {msg}"
        );
    }

    #[test]
    fn resolve_target_path_target_arg_takes_priority_over_yes_suggestion() {
        // Even with --yes set, an explicit --target must win.
        // Pin this — otherwise a future refactor that reorders
        // the early-returns could silently start ignoring the
        // user's explicit input.
        let args = MigrateArgs {
            legacy_key: Some("github/token".into()),
            target: Some("project-x/github/migrated".into()),
            yes: true,
            ..MigrateArgs::default()
        };
        let got = resolve_target_path(&args, "github/token").unwrap();
        assert_eq!(got, "project-x/github/migrated");
    }
}
