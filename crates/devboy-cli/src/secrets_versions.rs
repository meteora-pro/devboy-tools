//! `devboy secrets versions` and `devboy secrets restore` — undoing a
//! write (ADR-024 §5, Ф8-2).
//!
//! # The promise this makes good on
//!
//! §5 says a secret value is never destroyed by an agent-mediated
//! write: every write appends a version and the previous ciphertext
//! stays, so an agent that stores the wrong token — or an empty one —
//! is recoverable.
//!
//! Half of that was true. The vault did keep every version, and
//! `Vault::restore` did work. Neither was reachable: no command, no
//! RPC method, no tool called them, so a user whose agent had just
//! overwritten a token had no way to get it back. The promise was
//! real in the file format and absent from the product.
//!
//! # Why restoring is a person's job, not an agent's
//!
//! There is deliberately no MCP tool here. An agent that can undo its
//! own writes can also undo a *human's* correction of its writes, and
//! the recovery path stops being a backstop the moment the thing it
//! protects against can operate it. This is the same reasoning that
//! makes `purge` user-only.
//!
//! # What is shown, and what is not
//!
//! `versions` prints numbers, dates, who wrote each one, and which is
//! current. It never prints a value — reading a secret is what
//! `secrets get` is for, and a listing that quietly exposed values
//! would turn "let me see the history" into a disclosure.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use devboy_vault_crypto::vault::{UnlockMethod, Vault, VersionInfo};
use secrecy::SecretString;

/// Arguments for `devboy secrets versions <path>`.
#[derive(Args, Debug)]
pub struct VersionsArgs {
    /// ADR-020 path whose history to show.
    pub path: String,
}

/// Arguments for `devboy secrets restore <path>`.
#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// ADR-020 path to restore.
    pub path: String,

    /// Version to bring back. Omit to restore the one before the
    /// current version, which is what "undo that last write" means.
    #[arg(long)]
    pub version: Option<u64>,
}

/// Run `devboy secrets versions`.
pub fn run_versions(args: VersionsArgs) -> Result<()> {
    let vault = open_vault()?;
    let versions = vault.versions(&args.path);

    anyhow::ensure!(
        !versions.is_empty(),
        "no versions of `{}` in the vault. Check the path with `devboy secrets list`",
        args.path
    );

    println!("{}", render_versions(&args.path, &versions));
    Ok(())
}

/// Run `devboy secrets restore`.
pub fn run_restore(args: RestoreArgs) -> Result<()> {
    let mut vault = open_vault()?;
    let versions = vault.versions(&args.path);

    let target = match args.version {
        Some(v) => v,
        None => previous_version(&versions).with_context(|| {
            format!(
                "`{}` has no earlier version to go back to — there is nothing to undo",
                args.path
            )
        })?,
    };

    vault
        .restore(&args.path, target)
        .with_context(|| format!("could not restore `{}` to version {target}", args.path))?;

    println!(
        "Restored `{}` to version {target}. This appended a new version rather than rewriting \
         history, so the value you just replaced is still recoverable too.",
        args.path
    );
    Ok(())
}

/// Which version "undo the last write" means.
///
/// The one before the highest, skipping nothing: a tombstone is a
/// legitimate thing to go back to (someone deleted the secret and
/// wants that back), and silently stepping over it would restore a
/// value the user did not ask for.
///
/// Split out so the rule is testable without a vault on disk.
pub fn previous_version(versions: &[VersionInfo]) -> Option<u64> {
    if versions.len() < 2 {
        return None;
    }
    let mut numbers: Vec<u64> = versions.iter().map(|v| v.version).collect();
    numbers.sort_unstable();
    numbers.get(numbers.len() - 2).copied()
}

/// Render the history.
///
/// Built as a string so a test can assert on what is shown — and, as
/// importantly, on what is not.
pub fn render_versions(path: &str, versions: &[VersionInfo]) -> String {
    let current = versions.iter().map(|v| v.version).max().unwrap_or(0);

    let mut out = format!("{path}\n");
    for v in versions {
        let marker = if v.version == current { "*" } else { " " };
        let kind = if v.tombstone { "deleted" } else { "value" };
        let actor = v.actor.as_deref().unwrap_or("unknown");
        let when = v.created_at.as_deref().unwrap_or("date not recorded");
        out.push_str(&format!(
            "  {marker} v{:<4} {kind:<8} by {actor:<7} {when}\n",
            v.version
        ));
    }
    out.push_str("\n  * = current. Values are never shown here.\n");
    out.push_str(&format!(
        "  Undo the last write with `devboy secrets restore {path}`.\n"
    ));
    out
}

/// Open the vault, asking for the passphrase.
fn open_vault() -> Result<Vault> {
    let path = vault_path()?;
    anyhow::ensure!(
        path.exists(),
        "no vault at {}. Create one with the secrets UI first",
        path.display()
    );

    let passphrase = read_passphrase(&path)?;
    Vault::open(&path, UnlockMethod::Passphrase(passphrase))
        .with_context(|| format!("could not open {}", path.display()))
}

/// Where the vault lives.
fn vault_path() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("DEVBOY_VAULT_PATH") {
        return Ok(PathBuf::from(explicit));
    }
    let dir = dirs::config_dir().context("could not resolve the user's config directory")?;
    Ok(dir.join("devboy-tools").join("secrets").join("vault.dvb"))
}

/// Read the vault passphrase.
///
/// Honours `DEVBOY_VAULT_PASSPHRASE` for the same reason the keyfile
/// command does: recovery is exactly the situation where someone may
/// be driving this from a script on a machine with no terminal.
fn read_passphrase(vault_path: &std::path::Path) -> Result<SecretString> {
    use std::io::IsTerminal;

    if let Ok(from_env) = std::env::var("DEVBOY_VAULT_PASSPHRASE")
        && !from_env.is_empty()
    {
        return Ok(SecretString::from(from_env));
    }

    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "this needs an interactive terminal, or DEVBOY_VAULT_PASSPHRASE set"
    );

    let entered = dialoguer::Password::new()
        .with_prompt(format!("Passphrase for {}", vault_path.display()))
        .allow_empty_password(false)
        .interact()
        .context("could not read the passphrase")?;
    Ok(SecretString::from(entered))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(version: u64, tombstone: bool) -> VersionInfo {
        VersionInfo {
            version,
            tombstone,
            actor: Some("agent".into()),
            created_at: Some("2026-08-13T10:00:00Z".into()),
        }
    }

    /// "Undo the last write" is the version before the newest.
    #[test]
    fn the_previous_version_is_the_one_before_the_newest() {
        assert_eq!(
            previous_version(&[v(1, false), v(2, false), v(3, false)]),
            Some(2)
        );
    }

    /// A single version has nothing behind it, and saying so beats
    /// restoring the version that is already current.
    #[test]
    fn a_single_version_has_nothing_to_undo() {
        assert_eq!(previous_version(&[v(1, false)]), None);
        assert_eq!(previous_version(&[]), None);
    }

    /// A tombstone is a legitimate thing to go back to — someone
    /// deleted the secret and wants the deletion undone in turn.
    /// Skipping it would restore a value nobody asked for.
    #[test]
    fn a_tombstone_is_not_skipped() {
        assert_eq!(
            previous_version(&[v(1, false), v(2, true), v(3, false)]),
            Some(2)
        );
    }

    /// The listing exists to answer "what happened here", so it has
    /// to show who and when, and mark which one is live.
    #[test]
    fn the_listing_shows_who_wrote_what_and_which_is_current() {
        let out = render_versions("team/gitlab/token", &[v(1, false), v(2, false)]);

        assert!(out.contains("team/gitlab/token"), "{out}");
        assert!(out.contains("v1"), "{out}");
        assert!(out.contains("agent"), "{out}");
        assert!(out.contains("2026-08-13"), "{out}");
        assert!(
            out.contains("* v2") || out.contains("* v2   "),
            "the current version must be marked: {out}"
        );
    }

    #[test]
    fn a_deleted_version_is_labelled_as_such() {
        let out = render_versions("p", &[v(1, false), v(2, true)]);
        assert!(out.contains("deleted"), "{out}");
    }

    /// The listing must never become a way to read secrets.
    #[test]
    fn the_listing_says_plainly_that_it_shows_no_values() {
        let out = render_versions("p", &[v(1, false)]);
        assert!(out.contains("Values are never shown"), "{out}");
    }

    /// A user looking at history is usually about to undo something,
    /// and the command to do it should not need looking up.
    #[test]
    fn the_listing_names_the_command_that_undoes_a_write() {
        let out = render_versions("team/x/tok", &[v(1, false), v(2, false)]);
        assert!(out.contains("devboy secrets restore team/x/tok"), "{out}");
    }
}
