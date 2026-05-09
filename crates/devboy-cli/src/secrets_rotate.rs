//! `devboy secrets rotate <path>` per [ADR-023] §3.4 and
//! [ADR-021] §6.
//!
//! End-to-end rotation flow:
//!
//! 1. Resolve the manifest entry for `<path>`.
//! 2. Open `entry.retrieval_url` in the OS browser so the user
//!    can rotate the secret at the provider.
//! 3. Destructive-confirm (per ADR-023 §3.4: rotation must be
//!    explicitly acknowledged — overwriting an existing value
//!    is irreversible).
//! 4. Read the new value with a hidden, double-entry password
//!    prompt.
//! 5. Format-validate against `entry.format_regex` /
//!    `entry.pattern_id` (P9.1 / [`devboy_storage::validate_format`]).
//! 6. Update `entry.last_rotated_at` to today and persist the
//!    global index.
//! 7. Print clear next-steps. The framework records the
//!    rotation timestamp; the canonical value lives at the
//!    provider (the URL you opened in step 2).
//!
//! ## What this flow does **not** do
//!
//! Writing the new value back through the source router (1Password,
//! Vault, keychain) requires the `SecretSource::write` capability —
//! the trait surface for that ships in P15 (subprocess plugin
//! protocol). Until then the user updates the value at the provider
//! via the URL hand-off; this command tracks the metadata
//! (`last_rotated_at`) so `doctor` and the inventory view stop
//! flagging the secret as stale.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use clap::Args;
use devboy_secret_patterns::Catalogue;
use devboy_storage::{FormatCheck, GlobalIndex, IndexEntry, SecretPath, validate_format};
use secrecy::{ExposeSecret, SecretString};

// =============================================================================
// CLI surface
// =============================================================================

/// Flags for `devboy secrets rotate <path>`.
#[derive(Args, Debug, Default)]
pub struct RotateArgs {
    /// ADR-020 path of the secret to rotate (e.g.
    /// `team/jira/api-key`).
    pub path: String,
    /// Skip the OS-browser hand-off. Useful in CI / scripts that
    /// rotate values fetched out-of-band.
    #[arg(long)]
    pub no_browser: bool,
    /// Skip the destructive-confirm prompt. Required when stdin
    /// isn't a TTY (the prompt would have nothing to read).
    #[arg(long)]
    pub yes: bool,
    /// Read the new value from stdin (one line, no echo) instead
    /// of the interactive prompt. Useful for `vault read` /
    /// `op read` shell pipelines and for tests.
    #[arg(long)]
    pub from_stdin: bool,
    /// Override the path the global index is loaded from / saved
    /// to. Defaults to the platform's config dir.
    #[arg(long)]
    pub index: Option<PathBuf>,
}

/// Top-level handler. Wired from `secrets_cmd::handle`.
pub async fn handle(args: RotateArgs) -> Result<()> {
    let path = SecretPath::parse(&args.path)
        .with_context(|| format!("`{}` is not a valid ADR-020 path", args.path))?;

    let index_path = match args.index.as_ref() {
        Some(p) => p.clone(),
        None => GlobalIndex::default_path()
            .context("could not resolve the default global-index path")?,
    };
    let mut index = GlobalIndex::load_from(&index_path)
        .with_context(|| format!("could not load global index from {}", index_path.display()))?;

    let entry = index
        .get(&path)
        .ok_or_else(|| anyhow!("no entry for `{path}` in the global index"))?
        .clone();

    let catalogue = Catalogue::builtins_only();

    let opener = SystemBrowser;
    let prompts = real_prompts(&args);

    let outcome = run_rotation(
        &path,
        &entry,
        &args,
        &catalogue,
        &opener,
        prompts.as_ref(),
        TodayProvider::default(),
    )?;

    if outcome.recorded {
        index.record_rotation(&path, &outcome.today);
        index.save_to(&index_path).with_context(|| {
            format!("could not persist global index to {}", index_path.display())
        })?;
    }

    Ok(())
}

// =============================================================================
// Pure flow (testable)
// =============================================================================

#[derive(Debug, PartialEq, Eq)]
struct RotationOutcome {
    recorded: bool,
    today: String,
}

/// Executes the rotation flow against injected dependencies.
/// Pure over `index` access; the caller persists post-condition.
fn run_rotation(
    path: &SecretPath,
    entry: &IndexEntry,
    args: &RotateArgs,
    catalogue: &Catalogue,
    opener: &dyn UrlOpener,
    prompts: &dyn Prompts,
    today: TodayProvider,
) -> Result<RotationOutcome> {
    println!("Rotating `{path}`");
    if let Some(pattern_id) = entry.pattern_id.as_deref() {
        println!("  pattern_id: {pattern_id}");
    }
    if let Some(url) = entry.retrieval_url.as_deref() {
        println!("  upstream:   {url}");
    }

    if !args.no_browser {
        if let Some(url) = entry.retrieval_url.as_deref() {
            opener
                .open(url)
                .with_context(|| format!("could not open `{url}` in the OS browser"))?;
            println!("Opened `{url}` in your browser. Rotate the value there, then return here.");
        } else {
            eprintln!(
                "warning: no `retrieval_url` recorded for `{path}` — \
                 you'll need to update the value at the provider yourself."
            );
        }
    }

    if !args.yes && !prompts.confirm_destructive(path)? {
        println!("Aborted.");
        return Ok(RotationOutcome {
            recorded: false,
            today: today.today(),
        });
    }

    let value = prompts.read_secret_value()?;
    if value.expose_secret().is_empty() {
        anyhow::bail!("rotation aborted — empty value would clobber the recorded secret");
    }

    match validate_format(entry, value.expose_secret(), catalogue) {
        FormatCheck::Ok { source } => {
            println!("Format check OK ({source:?}).");
        }
        FormatCheck::NoRule => {
            println!("No format rule declared for this entry — skipping format check.");
        }
        FormatCheck::Mismatch { source, expected } => {
            anyhow::bail!(
                "rotation aborted — value does not match the declared format \
                 ({source:?}, expected `{expected}`). Re-run with the corrected value."
            );
        }
        FormatCheck::Error { message } => {
            anyhow::bail!("rotation aborted — format rule could not be evaluated: {message}");
        }
    }

    let today_str = today.today();
    println!("Recorded rotation timestamp: {today_str}");
    println!("Note: the value lives at the provider; this command did not write to it.");

    Ok(RotationOutcome {
        recorded: true,
        today: today_str,
    })
}

// =============================================================================
// Browser opener
// =============================================================================

trait UrlOpener {
    fn open(&self, url: &str) -> Result<()>;
}

struct SystemBrowser;

impl UrlOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<()> {
        let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
            ("open", vec![url])
        } else if cfg!(target_os = "windows") {
            ("cmd", vec!["/c", "start", "", url])
        } else {
            ("xdg-open", vec![url])
        };
        let status = Command::new(cmd)
            .args(&args)
            .status()
            .with_context(|| format!("failed to spawn `{cmd}`"))?;
        if !status.success() {
            anyhow::bail!("`{cmd}` exited with {status}");
        }
        Ok(())
    }
}

// =============================================================================
// Prompts
// =============================================================================

trait Prompts {
    fn confirm_destructive(&self, path: &SecretPath) -> Result<bool>;
    fn read_secret_value(&self) -> Result<SecretString>;
}

fn real_prompts(args: &RotateArgs) -> Box<dyn Prompts> {
    if args.from_stdin {
        Box::new(StdinPrompts)
    } else {
        Box::new(InteractivePrompts)
    }
}

struct InteractivePrompts;

impl Prompts for InteractivePrompts {
    fn confirm_destructive(&self, path: &SecretPath) -> Result<bool> {
        if !io::stdin().is_terminal() {
            anyhow::bail!(
                "rotation requires interactive confirmation; pass `--yes` for non-interactive runs"
            );
        }
        let prompt =
            format!("Rotation will overwrite the recorded value for `{path}`. Continue? [y/N] ");
        print!("{prompt}");
        io::stdout().flush().ok();
        let mut buf = String::new();
        io::stdin()
            .read_line(&mut buf)
            .context("could not read from stdin")?;
        let normalized = buf.trim().to_ascii_lowercase();
        Ok(normalized == "y" || normalized == "yes")
    }

    fn read_secret_value(&self) -> Result<SecretString> {
        let pw = dialoguer::Password::new()
            .with_prompt("New value")
            .with_confirmation("Re-enter to confirm", "Values do not match — start over")
            .interact()
            .context("password prompt failed")?;
        Ok(SecretString::new(pw.into()))
    }
}

struct StdinPrompts;

impl Prompts for StdinPrompts {
    fn confirm_destructive(&self, _path: &SecretPath) -> Result<bool> {
        Ok(true)
    }

    fn read_secret_value(&self) -> Result<SecretString> {
        let stdin = io::stdin();
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .context("could not read value from stdin")?;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        Ok(SecretString::new(trimmed.to_owned().into()))
    }
}

// =============================================================================
// Today provider
// =============================================================================

#[derive(Debug, Clone, Default)]
struct TodayProvider {
    fixed: Option<String>,
}

impl TodayProvider {
    #[cfg(test)]
    fn fixed(s: &str) -> Self {
        Self {
            fixed: Some(s.to_owned()),
        }
    }

    fn today(&self) -> String {
        match self.fixed.clone() {
            Some(s) => s,
            None => Utc::now().date_naive().format("%Y-%m-%d").to_string(),
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::Mutex;

    fn entry_with_format(regex: &str, retrieval: Option<&str>) -> IndexEntry {
        IndexEntry {
            format_regex: Some(regex.to_owned()),
            retrieval_url: retrieval.map(str::to_owned),
            pattern_id: Some("jira-api-token".to_owned()),
            ..IndexEntry::default()
        }
    }

    fn args() -> RotateArgs {
        RotateArgs {
            path: "team/jira/api-key".to_owned(),
            no_browser: true,
            yes: true,
            from_stdin: false,
            index: None,
        }
    }

    fn path() -> SecretPath {
        SecretPath::parse("team/jira/api-key").unwrap()
    }

    struct FakeBrowser {
        opened: Mutex<Vec<String>>,
        result: Result<(), String>,
    }

    impl FakeBrowser {
        fn ok() -> Self {
            Self {
                opened: Mutex::new(Vec::new()),
                result: Ok(()),
            }
        }
    }

    impl UrlOpener for FakeBrowser {
        fn open(&self, url: &str) -> Result<()> {
            self.opened.lock().unwrap().push(url.to_owned());
            self.result
                .clone()
                .map_err(|e| anyhow!("fake browser error: {e}"))
        }
    }

    struct FakePrompts {
        confirm: bool,
        value: RefCell<Option<String>>,
    }

    impl FakePrompts {
        fn new(confirm: bool, value: &str) -> Self {
            Self {
                confirm,
                value: RefCell::new(Some(value.to_owned())),
            }
        }
    }

    impl Prompts for FakePrompts {
        fn confirm_destructive(&self, _path: &SecretPath) -> Result<bool> {
            Ok(self.confirm)
        }

        fn read_secret_value(&self) -> Result<SecretString> {
            let v = self
                .value
                .borrow_mut()
                .take()
                .expect("read_secret_value called twice");
            Ok(SecretString::new(v.into()))
        }
    }

    fn cat() -> Catalogue {
        Catalogue::builtins_only()
    }

    // -- Format validation --------------------------------------

    #[test]
    fn rotation_succeeds_when_value_matches_format_regex() {
        let entry = entry_with_format(r"^[a-z0-9]{10}$", Some("https://example.invalid"));
        let prompts = FakePrompts::new(true, "abcdef0123");
        let browser = FakeBrowser::ok();
        let mut a = args();
        a.no_browser = true;
        let outcome = run_rotation(
            &path(),
            &entry,
            &a,
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap();
        assert!(outcome.recorded);
        assert_eq!(outcome.today, "2026-05-09");
    }

    #[test]
    fn rotation_aborts_when_value_does_not_match_format() {
        let entry = entry_with_format(r"^[a-z0-9]{10}$", None);
        let prompts = FakePrompts::new(true, "TOO-SHORT");
        let browser = FakeBrowser::ok();
        let err = run_rotation(
            &path(),
            &entry,
            &args(),
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not match the declared format"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rotation_skips_format_check_when_no_rule_declared() {
        let entry = IndexEntry {
            pattern_id: None,
            ..IndexEntry::default()
        };
        let prompts = FakePrompts::new(true, "anything-goes");
        let browser = FakeBrowser::ok();
        let outcome = run_rotation(
            &path(),
            &entry,
            &args(),
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap();
        assert!(outcome.recorded);
    }

    // -- Empty value guard --------------------------------------

    #[test]
    fn rotation_refuses_empty_value() {
        let entry = entry_with_format(r"^.+$", None);
        let prompts = FakePrompts::new(true, "");
        let browser = FakeBrowser::ok();
        let err = run_rotation(
            &path(),
            &entry,
            &args(),
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty value"), "{err}");
    }

    // -- Confirm gate -------------------------------------------

    #[test]
    fn user_can_back_out_at_destructive_confirm() {
        let entry = entry_with_format(r"^.+$", None);
        let prompts = FakePrompts::new(false, "any");
        let browser = FakeBrowser::ok();
        let mut a = args();
        a.yes = false;
        let outcome = run_rotation(
            &path(),
            &entry,
            &a,
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap();
        assert!(!outcome.recorded, "must NOT record when user aborted");
    }

    #[test]
    fn yes_flag_skips_destructive_confirm() {
        // FakePrompts.confirm = false would reject if asked, but
        // --yes makes us skip the question entirely.
        let entry = entry_with_format(r"^.+$", None);
        let prompts = FakePrompts::new(false, "abcdef");
        let browser = FakeBrowser::ok();
        let outcome = run_rotation(
            &path(),
            &entry,
            &args(),
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap();
        assert!(outcome.recorded);
    }

    // -- Browser open -------------------------------------------

    #[test]
    fn browser_opens_retrieval_url_when_present() {
        let entry = entry_with_format(r"^.+$", Some("https://example.invalid/jira"));
        let prompts = FakePrompts::new(true, "abcdef");
        let browser = FakeBrowser::ok();
        let mut a = args();
        a.no_browser = false;
        let _ = run_rotation(
            &path(),
            &entry,
            &a,
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap();
        let opened = browser.opened.lock().unwrap();
        assert_eq!(opened.as_slice(), &["https://example.invalid/jira"]);
    }

    #[test]
    fn no_browser_flag_skips_open_even_when_url_is_set() {
        let entry = entry_with_format(r"^.+$", Some("https://example.invalid/jira"));
        let prompts = FakePrompts::new(true, "abcdef");
        let browser = FakeBrowser::ok();
        let _ = run_rotation(
            &path(),
            &entry,
            &args(),
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap();
        assert!(browser.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn rotation_proceeds_when_no_retrieval_url_is_recorded() {
        let entry = entry_with_format(r"^.+$", None);
        let prompts = FakePrompts::new(true, "abcdef");
        let browser = FakeBrowser::ok();
        let mut a = args();
        a.no_browser = false;
        let outcome = run_rotation(
            &path(),
            &entry,
            &a,
            &cat(),
            &browser,
            &prompts,
            TodayProvider::fixed("2026-05-09"),
        )
        .unwrap();
        // Browser must NOT be called when no URL is set.
        assert!(browser.opened.lock().unwrap().is_empty());
        assert!(outcome.recorded);
    }

    // -- Today provider -----------------------------------------

    #[test]
    fn today_provider_default_returns_an_iso_8601_date() {
        let s = TodayProvider::default().today();
        assert_eq!(s.len(), 10);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
    }
}
