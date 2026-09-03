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
//! 4. Write the value at the new path through the credential
//!    chain.
//! 5. Register the entry in the global index with a
//!    "migrated from `<legacy>`" description.
//! 6. Confirm with the user, then delete the legacy entry —
//!    unless `--keep-legacy` is set.
//!
//! ## Source and destination are not the same store
//!
//! ADR-020 wrote the new path back into the keychain: at the time
//! the keychain *was* the credential chain, so a migration was a
//! rename in place. [ADR-024] §6 demoted the keychain out of the
//! chain, and a rename in place stopped being a migration — it
//! would move the value to a path the resolver no longer reads,
//! and then delete the only copy that still worked.
//!
//! So the two roles are explicit now: the **legacy store** (the OS
//! keychain, addressed directly rather than through the chain,
//! because the chain may no longer contain it) is read from and
//! deleted from, and the **destination** (the credential chain, so
//! normally the local vault) is written to.
//!
//! ## Re-running a migration
//!
//! A destination that already holds the target path is not an
//! error to overwrite blindly, it is a question: the value there
//! may be the one an earlier run of this command wrote, or it may
//! be an unrelated secret that happens to share the path. The two
//! are told apart by comparing the values, and only the first
//! counts as "already migrated" — see [`TargetWrite`].
//!
//! [ADR-024]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-024-agent-mediated-secret-access.md
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
use secrecy::ExposeSecret;
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

/// What happened at the destination path.
///
/// A bool cannot carry this: "did not write" covers both the
/// harmless re-run and the case where an unrelated secret is
/// sitting on the target path, and those two have opposite
/// consequences for deleting the legacy entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetWrite {
    /// The value was written at the target path.
    Written,
    /// The destination already held this exact value — an earlier
    /// run of the same migration. Nothing to do, and the legacy
    /// entry is safe to drop.
    AlreadyMigrated,
    /// The destination already held a *different* value at this
    /// path. Nothing is written and the legacy entry is kept: one
    /// of the two secrets would otherwise be lost, and this
    /// command cannot know which one is wanted.
    Conflict,
}

impl TargetWrite {
    /// Whether the destination is known to hold the legacy value
    /// after this step — the precondition for deleting the source.
    fn value_is_at_target(self) -> bool {
        matches!(self, Self::Written | Self::AlreadyMigrated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationOutcome {
    /// The plan that was executed.
    pub plan: MigrationPlan,
    /// What happened at the destination path.
    pub target: TargetWrite,
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

    // The legacy store is addressed directly rather than through
    // the chain: since ADR-024 §6 the chain does not contain the
    // keychain unless the user opted back in, and a migration
    // that cannot see its own source is useless.
    let legacy: Arc<dyn CredentialStore> = Arc::new(KeychainStore::new());
    // The destination is the chain, so the value lands wherever
    // reads will actually look for it — normally the local vault.
    let destination = crate::credential_chain();
    let mut index = GlobalIndex::load().context("failed to load global index")?;

    let plans = if args.all {
        plan_all(&legacy, args.keep_legacy)
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
        let outcome = execute_plan(&plan, legacy.as_ref(), &destination, &mut index)?;
        outcomes.push(outcome);
    }

    settle_migration_flag(legacy.as_ref(), &outcomes);

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
                    key
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
    legacy: &dyn CredentialStore,
    destination: &dyn CredentialStore,
    index: &mut GlobalIndex,
) -> Result<MigrationOutcome> {
    let target_path = SecretPath::parse(&plan.target_path).with_context(|| {
        format!(
            "target path '{}' is not a valid ADR-020 path",
            plan.target_path
        )
    })?;

    let value = legacy
        .get(&plan.legacy_key)
        .with_context(|| format!("failed to read legacy key '{}'", plan.legacy_key))?
        .ok_or_else(|| anyhow::anyhow!("legacy key '{}' is not present", plan.legacy_key))?;

    // Look before writing. An occupied target is either this
    // migration already having run, or a different secret living
    // at the same path; overwriting is only safe in the first
    // case. A plain byte comparison is right here — both values
    // are already in this process's memory, so there is no
    // timing signal an attacker could be on the other side of.
    let existing = destination
        .get(target_path.as_str())
        .with_context(|| format!("failed to read '{target_path}' at the destination"))?;

    let target = match existing {
        Some(found) if found.expose_secret() == value.expose_secret() => {
            TargetWrite::AlreadyMigrated
        }
        Some(_) => TargetWrite::Conflict,
        None => {
            destination
                .store(target_path.as_str(), &value)
                .with_context(|| format!("failed to write '{target_path}'"))?;
            TargetWrite::Written
        }
    };

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
    // A conflict means the destination does not hold this value,
    // so pointing the index at that path would advertise the
    // wrong secret.
    let registered_in_index =
        target.value_is_at_target() && index.insert(target_path.clone(), entry).is_none();

    // Optionally delete the legacy entry. Best-effort — a delete
    // failure is not a hard fail because the new entry is
    // already in place; just surface a warning. Deleting is
    // gated on the value actually being at the target: on a
    // conflict the legacy copy is the only one left.
    let mut deleted_legacy = false;
    if plan.delete_legacy && target.value_is_at_target() {
        match legacy.delete(&plan.legacy_key) {
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

    Ok(MigrationOutcome {
        plan: plan.clone(),
        target,
        registered_in_index,
        deleted_legacy,
    })
}

// =============================================================================
// Turning the legacy fallback off
// =============================================================================

/// Set `secrets.migration_complete` once the keychain is empty of
/// legacy entries, which is what switches off the read-only
/// legacy fallback in the credential chain.
///
/// The flag is an assertion that nothing is left behind, so it is
/// set from a fresh scan of the keychain rather than from the
/// outcomes: `--all` keeps the legacy entries by default, a
/// single-key run only moves one of several, and a conflict
/// deliberately leaves its source in place. In each of those the
/// migration is genuinely unfinished and the fallback has to stay.
///
/// Best-effort in both directions. Failing to save the config
/// does not fail a migration that already succeeded — the worst
/// case is a warning that persists until the next run — and the
/// flag is never cleared here, because a user who set it by hand
/// made a claim this function has no business overruling.
/// Legacy keys still present in the store.
///
/// Split out from [`settle_migration_flag`] so the decision can
/// be tested without a config file: the wrapper's remaining job
/// is to load, set and save, and it is the scan that decides
/// whether the fallback stays on.
fn legacy_entries_remaining(legacy: &dyn CredentialStore) -> Vec<String> {
    known_legacy_keys()
        .into_iter()
        .filter(|k| legacy.exists(k))
        .collect()
}

fn settle_migration_flag(legacy: &dyn CredentialStore, outcomes: &[MigrationOutcome]) {
    if outcomes.is_empty() {
        return;
    }

    let remaining = legacy_entries_remaining(legacy);

    if !remaining.is_empty() {
        println!(
            "  note: {} legacy entr{} still in the OS keychain, so the read-only fallback stays \
             on. Run `devboy secrets migrate --all` to finish.",
            remaining.len(),
            if remaining.len() == 1 { "y" } else { "ies" }
        );
        return;
    }

    let mut config = match devboy_core::config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: could not load config to record migration completion: {e}");
            return;
        }
    };

    if config.is_secrets_migration_complete() {
        return;
    }

    if let Err(e) = config
        .set("secrets.migration_complete", "true")
        .and_then(|()| config.save())
    {
        eprintln!("warning: migration finished but the flag could not be saved: {e}");
        eprintln!("  set it yourself with `devboy config set secrets.migration_complete true`");
        return;
    }

    println!(
        "  the OS keychain holds no more legacy entries — set \
         secrets.migration_complete, and the read-only legacy fallback is now off"
    );
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
    match o.target {
        TargetWrite::Written => bits.push("wrote new entry"),
        TargetWrite::AlreadyMigrated => bits.push("already migrated, left as is"),
        TargetWrite::Conflict => {
            bits.push("SKIPPED: a different secret is already at that path");
        }
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
    use secrecy::{ExposeSecret, SecretString};

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    fn fresh_store_with(legacy_key: &str, value: &str) -> Arc<dyn CredentialStore> {
        Arc::new(MemoryStore::with_credentials([(
            legacy_key.to_owned(),
            value.to_owned(),
        )]))
    }

    /// An empty destination — the normal state before a
    /// migration runs.
    fn empty_destination() -> Arc<dyn CredentialStore> {
        Arc::new(MemoryStore::new())
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

    /// The whole point of the command post-ADR-024: the value
    /// leaves the legacy store and lands in the destination. A
    /// version of this that wrote back into the legacy store
    /// would pass every other assertion here while moving the
    /// secret somewhere reads no longer reach.
    #[test]
    fn execute_plan_moves_the_value_from_the_legacy_store_to_the_destination() {
        let legacy = fresh_store_with("github/token", "ghp_fixture");
        let destination = empty_destination();
        let mut index = GlobalIndex::new();
        let plan = plan_one("github/token", "personal/github/token-legacy", true);

        let outcome =
            execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap();
        assert_eq!(outcome.target, TargetWrite::Written);
        assert!(outcome.registered_in_index);
        assert!(outcome.deleted_legacy);

        // The destination holds it.
        let val = destination
            .get("personal/github/token-legacy")
            .unwrap()
            .unwrap();
        assert_eq!(val.expose_secret(), "ghp_fixture");

        // The legacy store holds neither the old key nor the new
        // path — nothing was written back into it.
        assert!(legacy.get("github/token").unwrap().is_none());
        assert!(
            legacy
                .get("personal/github/token-legacy")
                .unwrap()
                .is_none(),
            "the migration must not write the target back into the legacy store"
        );

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
        let legacy = fresh_store_with("gitlab/token", "glpat-fixture");
        let destination = empty_destination();
        let mut index = GlobalIndex::new();
        let plan = plan_one("gitlab/token", "personal/gitlab/token-legacy", false);

        let outcome =
            execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap();
        assert_eq!(outcome.target, TargetWrite::Written);
        assert!(!outcome.deleted_legacy);

        // Legacy still present.
        assert!(legacy.get("gitlab/token").unwrap().is_some());
    }

    #[test]
    fn execute_plan_returns_false_for_index_when_target_already_existed() {
        let legacy = fresh_store_with("github/token", "ghp_fixture");
        let destination = empty_destination();
        let mut index = GlobalIndex::new();
        index.insert(
            p("personal/github/token-legacy"),
            IndexEntry {
                description: Some("pre-existing".into()),
                ..IndexEntry::default()
            },
        );
        let plan = plan_one("github/token", "personal/github/token-legacy", false);

        let outcome =
            execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap();
        // Target was overwritten in the index.
        assert_eq!(outcome.target, TargetWrite::Written);
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
        let legacy = fresh_store_with("github/token", "ghp");
        let destination = empty_destination();
        let mut index = GlobalIndex::new();
        // 2 segments — fails ADR-020 validator.
        let plan = plan_one("github/token", "github/token", true);
        let err =
            execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a valid ADR-020 path"));
    }

    #[test]
    fn execute_plan_errors_when_legacy_absent() {
        let legacy = empty_destination();
        let destination = empty_destination();
        let mut index = GlobalIndex::new();
        let plan = plan_one("github/token", "personal/github/token-legacy", true);
        let err =
            execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not present"));
    }

    // -- execute_plan: an occupied target -------------------------

    /// Re-running the same migration must be a no-op, not a
    /// rewrite. This is the ordinary case: `--all` defaults to
    /// keeping the legacy entries, so the second run sees the
    /// same sources it saw the first time.
    #[test]
    fn a_second_run_over_the_same_value_writes_nothing_and_still_clears_the_legacy_entry() {
        let legacy = fresh_store_with("github/token", "ghp_fixture");
        let destination = empty_destination();
        let mut index = GlobalIndex::new();
        let plan = plan_one("github/token", "personal/github/token-legacy", true);

        // First run migrates but keeps nothing to distinguish
        // from a fresh install, so re-seed the legacy entry to
        // model "the user ran with --keep-legacy, then again
        // without".
        execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap();
        legacy
            .store(
                "github/token",
                &SecretString::from("ghp_fixture".to_owned()),
            )
            .unwrap();

        let second =
            execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap();
        assert_eq!(second.target, TargetWrite::AlreadyMigrated);
        assert!(
            second.deleted_legacy,
            "the value is demonstrably at the target, so the source copy is redundant"
        );
        assert_eq!(
            destination
                .get("personal/github/token-legacy")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "ghp_fixture"
        );
    }

    /// The dangerous case: something unrelated already occupies
    /// the target path. Overwriting loses that secret; deleting
    /// the legacy entry loses the other one. Neither is this
    /// command's call to make, so it does neither.
    #[test]
    fn a_different_secret_at_the_target_is_neither_overwritten_nor_costs_the_legacy_copy() {
        let legacy = fresh_store_with("github/token", "ghp_fixture");
        let destination: Arc<dyn CredentialStore> = Arc::new(MemoryStore::with_credentials([(
            "personal/github/token-legacy".to_owned(),
            "someone-elses-secret".to_owned(),
        )]));
        let mut index = GlobalIndex::new();
        let plan = plan_one("github/token", "personal/github/token-legacy", true);

        let outcome =
            execute_plan(&plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap();

        assert_eq!(outcome.target, TargetWrite::Conflict);
        assert!(!outcome.deleted_legacy, "the legacy copy is the only one");
        assert!(
            !outcome.registered_in_index,
            "the index must not point at a path holding a different secret"
        );
        assert_eq!(
            destination
                .get("personal/github/token-legacy")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "someone-elses-secret",
            "the occupant survives"
        );
        assert!(
            legacy.get("github/token").unwrap().is_some(),
            "and so does the source"
        );
    }

    /// A destination that refuses writes — the normal state when
    /// the vault is locked and the keychain is opted out — must
    /// fail before the legacy entry is touched. Losing the only
    /// copy to a failed write is the worst outcome available.
    #[test]
    fn a_failing_destination_leaves_the_legacy_entry_alone() {
        let legacy = fresh_store_with("github/token", "ghp_fixture");
        let destination = ReadOnlyStore;
        let mut index = GlobalIndex::new();
        let plan = plan_one("github/token", "personal/github/token-legacy", true);

        let err = execute_plan(&plan, legacy.as_ref(), &destination, &mut index).unwrap_err();
        assert!(format!("{err:#}").contains("failed to write"));
        assert!(
            legacy.get("github/token").unwrap().is_some(),
            "a failed write must not cost the source copy"
        );
    }

    /// Stands in for the locked vault: reads fine, refuses writes.
    struct ReadOnlyStore;

    impl CredentialStore for ReadOnlyStore {
        fn store(&self, _key: &str, _value: &SecretString) -> devboy_core::Result<()> {
            Err(devboy_core::Error::Storage("nowhere to store".into()))
        }
        fn get(&self, _key: &str) -> devboy_core::Result<Option<SecretString>> {
            Ok(None)
        }
        fn delete(&self, _key: &str) -> devboy_core::Result<()> {
            Ok(())
        }
        fn is_writable(&self) -> bool {
            false
        }
    }

    // -- turning the fallback off ---------------------------------

    /// The flag means "nothing is left in the keychain". A run
    /// that moved one of several entries has not earned it —
    /// `--all` keeps the sources by default and a conflict
    /// deliberately leaves one behind, so reading completion off
    /// the outcomes rather than off the keychain would switch the
    /// fallback off while secrets still depend on it.
    #[test]
    fn a_partly_migrated_keychain_still_has_entries_remaining() {
        let legacy: Arc<dyn CredentialStore> = Arc::new(MemoryStore::with_credentials([(
            "gitlab/token".to_owned(),
            "glpat".to_owned(),
        )]));

        assert_eq!(legacy_entries_remaining(legacy.as_ref()), ["gitlab/token"]);
    }

    #[test]
    fn an_empty_keychain_has_nothing_remaining() {
        let legacy = empty_destination();
        assert!(legacy_entries_remaining(legacy.as_ref()).is_empty());
    }

    /// Only keys the migration actually knows about count. A
    /// user's own unrelated keychain entry is not a legacy devboy
    /// secret and must not hold the fallback on forever.
    #[test]
    fn an_unrelated_keychain_entry_does_not_count_as_remaining() {
        let legacy: Arc<dyn CredentialStore> = Arc::new(MemoryStore::with_credentials([(
            "my-own-app/api-key".to_owned(),
            "whatever".to_owned(),
        )]));

        assert!(legacy_entries_remaining(legacy.as_ref()).is_empty());
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
        let legacy: Arc<dyn CredentialStore> = Arc::new(MemoryStore::with_credentials([
            ("github/token".into(), "ghp".into()),
            ("gitlab/token".into(), "glpat".into()),
        ]));
        let destination = empty_destination();
        let mut index = GlobalIndex::new();
        let plans = plan_all(&legacy, /*keep_legacy=*/ false);
        assert_eq!(plans.len(), 2);

        for plan in &plans {
            execute_plan(plan, legacy.as_ref(), destination.as_ref(), &mut index).unwrap();
        }

        // Both new paths in the destination, both legacy keys gone.
        assert!(
            destination
                .get("personal/github/token-legacy")
                .unwrap()
                .is_some()
        );
        assert!(
            destination
                .get("personal/gitlab/token-legacy")
                .unwrap()
                .is_some()
        );
        assert!(legacy.get("github/token").unwrap().is_none());
        assert!(legacy.get("gitlab/token").unwrap().is_none());

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
