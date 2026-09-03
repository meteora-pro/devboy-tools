//! `devboy secrets add-totp` — enrol an authenticator
//! (ADR-024 §1, Ф6d-2).
//!
//! # What enrolment actually establishes
//!
//! The point of TOTP here is not a second factor in the usual
//! sense. It is a way for a *human* to approve a re-unlock that an
//! agent cannot approve on its own: the shared secret lives in
//! daemon memory and in the vault's reserved slot, both unreachable
//! from the agent-facing surface, so a valid code is evidence that
//! somebody looked at a phone.
//!
//! # One chance to record it
//!
//! Ф6c closed `__totp/secret` to reads, and that includes this
//! command on any later run. The secret is displayed once, at
//! enrolment, and there is no way to ask for it again — a lost
//! authenticator means enrolling a fresh secret, not recovering the
//! old one. The output says so plainly, because a user who assumes
//! otherwise finds out at the worst possible moment.

use anyhow::{Context, Result};
use clap::Args;
use devboy_vault_crypto::totp;
use secrecy::SecretString;

/// Bytes of TOTP secret to generate.
///
/// RFC 4226 §4 requires at least 128 bits and recommends 160; 256
/// costs nothing here and leaves no argument about margin.
const SECRET_BYTES: usize = 32;

/// Arguments for `devboy secrets add-totp`.
#[derive(Args, Debug, Default)]
pub struct AddTotpArgs {
    /// Label shown in the authenticator app.
    #[arg(long, default_value = "devboy")]
    pub issuer: String,

    /// Account name shown in the authenticator app. Defaults to the
    /// current user.
    #[arg(long)]
    pub account: Option<String>,

    /// Print the `otpauth://` URI and secret without drawing a QR
    /// code. Useful over SSH, in a pipe, or when the terminal
    /// mangles block characters.
    #[arg(long)]
    pub no_qr: bool,
}

/// Render a QR code as text.
///
/// Two terminal rows are packed into one line using half-block
/// characters, because a QR drawn one module per row is usually
/// taller than the window and unscannable.
pub fn render_qr(data: &str) -> Result<String> {
    let code = qrcode::QrCode::new(data.as_bytes()).context("could not encode the QR code")?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

/// Generate a secret, enrol it, and show it to the user once.
///
/// Returns the generated secret so the caller can hand it to the
/// vault; kept separate from the I/O so the format is testable
/// without a terminal.
pub fn generate_secret() -> Result<Vec<u8>> {
    let mut secret = vec![0u8; SECRET_BYTES];
    getrandom::getrandom(&mut secret).context("no secure randomness available")?;
    Ok(secret)
}

/// The full enrolment message, assembled for display.
///
/// Built as a string rather than printed piecemeal so a test can
/// assert what the user is told — particularly the part about this
/// being the only time they will see it.
pub fn enrolment_message(secret: &[u8], issuer: &str, account: &str, qr: Option<&str>) -> String {
    let uri = totp::provisioning_uri(secret, issuer, account);
    let base32 = data_encoding::BASE32_NOPAD.encode(secret);

    let mut out = String::new();
    out.push_str("Authenticator enrolled.\n\n");

    if let Some(qr) = qr {
        out.push_str(qr);
        out.push('\n');
    }

    out.push_str("Scan the code above, or enter this key by hand:\n\n");
    out.push_str(&format!("  {base32}\n\n"));
    out.push_str("Setup URI:\n\n");
    out.push_str(&format!("  {uri}\n\n"));
    out.push_str(
        "This is the only time the key is shown. It cannot be read back — not by you, not by \
         an agent, not by this command on a later run, which is exactly what makes a code from \
         it worth something. If you lose the authenticator, enrol a new one.\n",
    );
    out
}

/// Run `devboy secrets add-totp`.
pub fn handle(args: AddTotpArgs) -> Result<()> {
    let account = match args.account {
        Some(a) => a,
        None => std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".to_owned()),
    };

    let secret = generate_secret()?;

    // Enrol before showing anything: a user who scanned a QR for a
    // secret that then failed to persist would have an
    // authenticator entry that never works.
    enrol(&secret).context("could not enrol the authenticator")?;

    let qr = if args.no_qr {
        None
    } else {
        render_qr(&totp::provisioning_uri(&secret, &args.issuer, &account)).ok()
    };

    println!(
        "{}",
        enrolment_message(&secret, &args.issuer, &account, qr.as_deref())
    );
    Ok(())
}

/// Write the secret into the vault's reserved slot and add the
/// matching envelope.
///
/// Goes through the vault directly rather than the daemon socket,
/// because the daemon refuses writes to reserved paths — deliberately,
/// so an agent cannot swap the secret for one of its own. Enrolment
/// is a user action at the vault itself.
fn enrol(secret: &[u8]) -> Result<()> {
    use devboy_vault_crypto::vault::{UnlockMethod, Vault};

    let vault_path = vault_path()?;
    anyhow::ensure!(
        vault_path.exists(),
        "no vault at {} — create one with `devboy secrets ui` before enrolling an authenticator",
        vault_path.display()
    );

    let passphrase = read_passphrase(&vault_path)?;
    let mut vault = Vault::open(&vault_path, UnlockMethod::Passphrase(passphrase))
        .with_context(|| format!("could not open {}", vault_path.display()))?;

    vault
        .put(
            devboy_secrets_agent::TOTP_SECRET_PATH,
            &SecretString::from(data_encoding::BASE32_NOPAD.encode(secret)),
            Default::default(),
        )
        .context("could not store the shared secret")?;
    vault
        .add_totp_envelope(secret)
        .context("could not add the TOTP unlock envelope")?;

    Ok(())
}

/// Where the vault lives.
fn vault_path() -> Result<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("DEVBOY_VAULT_PATH") {
        return Ok(std::path::PathBuf::from(explicit));
    }
    // Shared with the daemon and the index rather than rebuilt here:
    // a second copy of this path is a second thing to keep in step.
    devboy_core::config::Config::vault_path()
        .context("could not resolve the user's config directory")
}

/// Read the vault passphrase, refusing to run without a terminal.
///
/// Enrolment is a deliberate user action, so there is no
/// non-interactive path: a pipe here would mean the passphrase came
/// from somewhere scriptable, which is what this whole scheme is
/// trying to avoid.
fn read_passphrase(vault_path: &std::path::Path) -> Result<SecretString> {
    use std::io::IsTerminal;

    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "enrolling an authenticator needs an interactive terminal; it will not read the \
         passphrase from a pipe"
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

    #[test]
    fn the_generated_secret_is_long_enough_to_matter() {
        let secret = generate_secret().expect("generate");
        // RFC 4226 §4 requires at least 128 bits; assert the
        // generated length rather than the constant, so shrinking
        // the constant is what fails.
        assert_eq!(secret.len(), SECRET_BYTES);
        assert!(
            secret.len() * 8 >= 128,
            "a TOTP secret shorter than 128 bits is below RFC 4226 §4"
        );
    }

    #[test]
    fn two_generated_secrets_differ() {
        let a = generate_secret().expect("a");
        let b = generate_secret().expect("b");
        assert_ne!(a, b, "enrolment must not produce a predictable secret");
    }

    /// The user gets exactly one chance to record the key, and the
    /// message has to say so — someone who assumes otherwise finds
    /// out when their phone is already wiped.
    #[test]
    fn the_message_warns_that_the_key_is_shown_once() {
        let msg = enrolment_message(b"0123456789abcdef0123", "devboy", "alice", None);
        assert!(
            msg.contains("only time"),
            "the one-shot nature must be stated: {msg}"
        );
        assert!(
            msg.contains("cannot be read back"),
            "the message should say the key is unrecoverable: {msg}"
        );
    }

    /// Both enrolment routes must be present: not every terminal
    /// renders a QR, and not every user can scan one.
    #[test]
    fn the_message_offers_both_a_key_and_a_uri() {
        let secret = b"0123456789abcdef0123";
        let msg = enrolment_message(secret, "devboy", "alice", None);

        let base32 = data_encoding::BASE32_NOPAD.encode(secret);
        assert!(msg.contains(&base32), "the manual-entry key must appear");
        assert!(msg.contains("otpauth://totp/"), "the setup URI must appear");
    }

    /// The URI has to be the one the crypto layer produces, or the
    /// authenticator and the verifier disagree about the secret.
    #[test]
    fn the_uri_matches_the_crypto_layers_own() {
        let secret = b"0123456789abcdef0123";
        let msg = enrolment_message(secret, "devboy", "alice", None);
        assert!(msg.contains(&totp::provisioning_uri(secret, "devboy", "alice")));
    }

    #[test]
    fn a_qr_is_included_when_one_was_rendered() {
        let msg = enrolment_message(b"0123456789abcdef0123", "devboy", "alice", Some("QRHERE"));
        assert!(msg.contains("QRHERE"));
    }

    /// The QR has to encode the URI itself — a code that scans to
    /// something else is worse than no code.
    #[test]
    fn the_qr_encodes_the_provisioning_uri() {
        let uri = totp::provisioning_uri(b"0123456789abcdef0123", "devboy", "alice");
        let rendered = render_qr(&uri).expect("render");

        assert!(!rendered.is_empty());
        // Round-trip through the encoder: the same input must
        // produce the same modules, and a different one must not.
        let same = render_qr(&uri).expect("render again");
        assert_eq!(rendered, same);
        assert_ne!(rendered, render_qr("otpauth://totp/other").expect("other"));
    }

    /// A URI long enough to need a bigger QR version must still
    /// encode rather than failing the enrolment.
    #[test]
    fn a_long_uri_still_renders() {
        let uri = totp::provisioning_uri(
            &[7u8; 64],
            "a-rather-long-issuer-name",
            "someone@example.com",
        );
        assert!(
            render_qr(&uri).is_ok(),
            "enrolment must not fail on a long label"
        );
    }
}
