//! `devboy secrets keyfile` — enrol, inspect and remove the keyfile
//! that lets a vault open unattended (ADR-024 §6, Ф7-2).
//!
//! # Why this exists
//!
//! The keyfile envelope was implemented, the daemon could unlock with
//! it, and the configuration field to point at it was in place. What
//! was missing was any way to *create* one: `add_keyfile_envelope`
//! had no caller outside the crate's own tests. The daemon's own
//! error text told users to run `devboy secrets keyfile add` — a
//! command that did not exist.
//!
//! So unattended cold start was unreachable end to end, which is
//! worth stating plainly because everything downstream of it —
//! machine binding included — was sitting on a feature nobody could
//! turn on.
//!
//! # Enrolment is a second door, not a way in
//!
//! Adding a keyfile requires opening the vault first, by passphrase,
//! at a terminal. The keyfile does not grant access; it records a new
//! way to reach access the user already had. That ordering is what
//! keeps "enrol a keyfile" from being a privilege escalation for
//! anything that can run a command as the user.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use devboy_vault_crypto::format::{Envelope, VaultFile};
use devboy_vault_crypto::keyfile;
use devboy_vault_crypto::vault::{UnlockMethod, Vault};
use secrecy::SecretString;

/// `devboy secrets keyfile <subcommand>`.
#[derive(Args, Debug)]
pub struct KeyfileArgs {
    #[command(subcommand)]
    pub command: KeyfileCommands,
}

#[derive(Subcommand, Debug)]
pub enum KeyfileCommands {
    /// Generate a keyfile and enrol it, so the vault can open with
    /// no human present.
    Add(AddArgs),
    /// Report whether a keyfile is enrolled and usable.
    Status,
    /// Un-enrol the keyfile. The file on disk is left alone.
    Remove,
}

#[derive(Args, Debug, Default)]
pub struct AddArgs {
    /// Where to write the keyfile. Defaults to the platform state
    /// directory, deliberately outside the config tree that holds
    /// the vault.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// Enrol an existing file instead of generating one. Use this
    /// when the key comes from somewhere else — a secrets mount, a
    /// hardware-backed file, an orchestrator.
    #[arg(long)]
    pub use_existing: bool,
}

/// Run `devboy secrets keyfile`.
pub fn run(args: KeyfileArgs) -> Result<()> {
    match args.command {
        KeyfileCommands::Add(add) => run_add(add),
        KeyfileCommands::Status => run_status(),
        KeyfileCommands::Remove => run_remove(),
    }
}

fn run_add(args: AddArgs) -> Result<()> {
    let vault_path = vault_path()?;
    anyhow::ensure!(
        vault_path.exists(),
        "no vault at {}. Create one with `devboy secrets setup` first",
        vault_path.display()
    );

    let target = match args.path {
        Some(p) => p,
        None => keyfile::default_keyfile_path()
            .context("could not resolve a default keyfile location; pass --path")?,
    };

    let bytes = if args.use_existing {
        anyhow::ensure!(
            target.exists(),
            "--use-existing was given but {} does not exist",
            target.display()
        );
        keyfile::load_keyfile(&target)
            .with_context(|| format!("could not use {}", target.display()))?
    } else {
        // Refuse to clobber. Overwriting silently would strand
        // whatever the old file still opens, and the user would
        // discover that only at the next unattended start.
        anyhow::ensure!(
            !target.exists(),
            "{} already exists. Enrol it with `--use-existing`, or pass `--path` to write \
             somewhere else",
            target.display()
        );
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        keyfile::create_keyfile(&target)
            .with_context(|| format!("could not create {}", target.display()))?
    };

    let passphrase = read_passphrase(&vault_path)?;
    let mut vault = Vault::open(&vault_path, UnlockMethod::Passphrase(passphrase))
        .with_context(|| format!("could not open {}", vault_path.display()))?;

    vault
        .add_keyfile_envelope(&bytes)
        .context("could not add the keyfile unlock envelope")?;

    record_keyfile_path(&target)?;

    let bound = binding_of(&vault_path).unwrap_or(None);
    println!("{}", enrolment_message(&target, bound.as_deref()));
    Ok(())
}

fn run_status() -> Result<()> {
    let vault_path = vault_path()?;
    let configured = devboy_core::config::Config::load()
        .ok()
        .and_then(|c| c.secrets_keyfile_path().map(Path::to_path_buf));

    let enrolled = binding_of(&vault_path);
    println!(
        "{}",
        status_message(&vault_path, configured.as_deref(), enrolled)
    );
    Ok(())
}

fn run_remove() -> Result<()> {
    let vault_path = vault_path()?;
    let passphrase = read_passphrase(&vault_path)?;
    let mut vault = Vault::open(&vault_path, UnlockMethod::Passphrase(passphrase))
        .with_context(|| format!("could not open {}", vault_path.display()))?;

    if vault.remove_keyfile_envelope()? {
        println!(
            "Keyfile un-enrolled. The file itself was left in place — delete it yourself once you \
             are sure nothing else uses it."
        );
    } else {
        println!("No keyfile was enrolled; nothing to remove.");
    }
    Ok(())
}

/// Read the vault's keyfile envelope binding without unlocking it.
///
/// `Ok(None)` means enrolled but unbound; `Err(())` means no keyfile
/// envelope at all. Distinguishing the two is the whole value of the
/// status output.
#[allow(clippy::result_unit_err)]
pub fn binding_of(vault_path: &Path) -> std::result::Result<Option<String>, ()> {
    let file = VaultFile::read_file(vault_path).map_err(|_| ())?;
    file.envelopes
        .iter()
        .find_map(|e| match e {
            Envelope::Keyfile {
                machine_binding, ..
            } => Some(machine_binding.clone()),
            _ => None,
        })
        .ok_or(())
}

/// What the user is told after a successful enrolment.
///
/// Built as a string so a test can assert on it: the two things that
/// must be said are where the file is and whether it is tied to this
/// machine, because both change what a backup of it is worth.
pub fn enrolment_message(path: &Path, binding: Option<&str>) -> String {
    let mut out = format!(
        "Keyfile enrolled.\n\n  file:   {}\n  config: secrets.keyfile_path updated\n",
        path.display()
    );

    match binding {
        Some(scheme) => out.push_str(&format!(
            "  bound:  yes ({scheme})\n\nThis vault will not open with this keyfile on any other \
             machine. Copying both files to a new host — a restored backup, a synced home \
             directory, a container image — will not work there; enrol again on that machine \
             instead.\n"
        )),
        None => out.push_str(
            "  bound:  no\n\nNo stable machine identifier was found here, so the keyfile is not \
             tied to this host. Anyone who obtains both the vault and this file can open it \
             anywhere. Keep them in separate places.\n",
        ),
    }

    out.push_str(
        "\nThe keyfile is as good as the vault: anything that can read it can open your secrets. \
         It is readable only by you (0600) and lives outside the config directory so a backup of \
         one does not carry the other.\n",
    );
    out
}

/// The `status` report.
pub fn status_message(
    vault_path: &Path,
    configured: Option<&Path>,
    enrolled: std::result::Result<Option<String>, ()>,
) -> String {
    let mut out = String::new();

    match enrolled {
        Err(()) => {
            out.push_str("Keyfile: not enrolled\n");
            out.push_str(
                "\nThe vault cannot be opened without a human. Run `devboy secrets keyfile add` \
                 to allow an unattended start.\n",
            );
            return out;
        }
        Ok(Some(scheme)) => {
            out.push_str(&format!(
                "Keyfile: enrolled, bound to this machine ({scheme})\n"
            ));
        }
        Ok(None) => {
            out.push_str("Keyfile: enrolled, not bound to a machine\n");
        }
    }

    out.push_str(&format!("  vault:  {}\n", vault_path.display()));

    match configured {
        Some(path) if path.exists() => {
            out.push_str(&format!("  file:   {} (present)\n", path.display()));
        }
        Some(path) => {
            out.push_str(&format!("  file:   {} (MISSING)\n", path.display()));
            out.push_str(
                "\nThe configured keyfile is not there, so an unattended start will fail. Restore \
                 the file, or enrol a new one with `devboy secrets keyfile add`.\n",
            );
        }
        None => {
            out.push_str("  file:   not configured\n");
            out.push_str(
                "\nAn envelope is enrolled but `secrets.keyfile_path` is unset, so the daemon does \
                 not know which file to read. Set it, or re-run `devboy secrets keyfile add`.\n",
            );
        }
    }

    out
}

/// Persist the keyfile location in the global config.
///
/// The daemon reads the path from configuration and never from a
/// request, so enrolment is only half-done until this is written —
/// which is exactly the kind of missing half this command exists to
/// stop happening.
fn record_keyfile_path(path: &Path) -> Result<()> {
    let mut config = devboy_core::config::Config::load().unwrap_or_default();
    let mut secrets = config.secrets.clone().unwrap_or_default();
    secrets.keyfile_path = Some(path.to_path_buf());
    config.secrets = Some(secrets);
    config
        .save()
        .context("could not save the keyfile path to the config")
}

/// Where the vault lives.
fn vault_path() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("DEVBOY_VAULT_PATH") {
        return Ok(PathBuf::from(explicit));
    }
    let dir = dirs::config_dir().context("could not resolve the user's config directory")?;
    Ok(dir.join("devboy-tools").join("secrets").join("vault.dvb"))
}

/// Environment variable holding the vault passphrase for an
/// unattended run.
///
/// Already the project's convention (`devboy secrets selftest` names
/// it, and the TUI honours it), so a second mechanism here would just
/// be a thing to get wrong.
const PASSPHRASE_ENV: &str = "DEVBOY_VAULT_PASSPHRASE";

/// Read the vault passphrase.
///
/// Unlike authenticator enrolment, this one accepts
/// `DEVBOY_VAULT_PASSPHRASE`, and the reason is the command's whole
/// purpose: a keyfile exists so a vault can open where no human is
/// present. Requiring a terminal to enrol one would mean the feature
/// could never be set up in the environment it was built for —
/// a container image, a provisioning script, a CI runner.
///
/// The trade is real and worth naming: a passphrase in the
/// environment is readable through `/proc/<pid>/environ` by anything
/// running as the same user. That is the same user who can read the
/// keyfile itself, so it widens no boundary here, but it is not a
/// habit to carry to an interactive machine.
fn read_passphrase(vault_path: &Path) -> Result<SecretString> {
    use std::io::IsTerminal;

    if let Ok(from_env) = std::env::var(PASSPHRASE_ENV)
        && !from_env.is_empty()
    {
        return Ok(SecretString::from(from_env));
    }

    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "enrolling a keyfile needs an interactive terminal, or {PASSPHRASE_ENV} set for an \
         unattended run"
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

    /// A bound keyfile and an unbound one are worth different things
    /// in a backup, so the message has to distinguish them.
    #[test]
    fn the_enrolment_message_says_whether_it_is_machine_bound() {
        let bound = enrolment_message(
            Path::new("/home/u/.local/state/vault.key"),
            Some("machine-v1"),
        );
        assert!(bound.contains("bound:  yes"), "{bound}");
        assert!(
            bound.contains("will not open with this keyfile on any other machine"),
            "{bound}"
        );

        let unbound = enrolment_message(Path::new("/home/u/.local/state/vault.key"), None);
        assert!(unbound.contains("bound:  no"), "{unbound}");
        assert!(
            unbound.contains("can open it anywhere"),
            "an unbound keyfile must not read as protected: {unbound}"
        );
    }

    /// The path matters — a user who cannot find the file cannot
    /// back it up or exclude it from a backup.
    #[test]
    fn the_enrolment_message_names_the_file() {
        let msg = enrolment_message(Path::new("/tmp/x/vault.key"), Some("machine-v1"));
        assert!(msg.contains("/tmp/x/vault.key"), "{msg}");
    }

    #[test]
    fn status_reports_a_vault_with_no_keyfile() {
        let msg = status_message(Path::new("/v/vault.dvb"), None, Err(()));
        assert!(msg.contains("not enrolled"), "{msg}");
        assert!(msg.contains("devboy secrets keyfile add"), "{msg}");
    }

    /// The two halves can drift apart — an envelope with no
    /// configured path is enrolled and unusable, and saying only
    /// "enrolled" would be a lie by omission.
    #[test]
    fn status_flags_an_envelope_with_no_configured_path() {
        let msg = status_message(
            Path::new("/v/vault.dvb"),
            None,
            Ok(Some("machine-v1".into())),
        );
        assert!(msg.contains("not configured"), "{msg}");
        assert!(msg.contains("does not know which file to read"), "{msg}");
    }

    #[test]
    fn status_flags_a_configured_but_missing_file() {
        let msg = status_message(
            Path::new("/v/vault.dvb"),
            Some(Path::new("/definitely/not/here.key")),
            Ok(None),
        );
        assert!(msg.contains("MISSING"), "{msg}");
        assert!(msg.contains("unattended start will fail"), "{msg}");
    }

    #[test]
    fn status_distinguishes_bound_from_unbound() {
        let bound = status_message(
            Path::new("/v/vault.dvb"),
            Some(Path::new("/v/k.key")),
            Ok(Some("machine-v1".into())),
        );
        assert!(bound.contains("bound to this machine"), "{bound}");

        let unbound = status_message(
            Path::new("/v/vault.dvb"),
            Some(Path::new("/v/k.key")),
            Ok(None),
        );
        assert!(unbound.contains("not bound to a machine"), "{unbound}");
    }
}
