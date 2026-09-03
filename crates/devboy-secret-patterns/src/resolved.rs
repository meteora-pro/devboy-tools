//! Where user pattern files live, and how a bad one is handled.
//!
//! # Why this module exists
//!
//! [`Catalogue::load`] was written, tested and never called. A user
//! could write a pattern file in the documented TOML format, get it
//! validated, get warned about shadowing a built-in — and it applied
//! nowhere: not in the audit scrubber, not in the proxy's response
//! scrubbing, not in format validation, not in rotation. Every
//! production caller asked for [`Catalogue::builtins_only`].
//!
//! Wiring it up needed a decision the code did not contain: where do
//! the files live, and what happens when one of them is malformed.
//! Both are here, in one place, because six callers each answering
//! them separately is how they come to disagree. The directory name
//! was not even the open question — [`USER_PATTERNS_SUBDIR`] has
//! existed all along, and the module docs name the path. Nothing
//! ever joined it to a config directory.
//!
//! # The directory
//!
//! `<config>/secrets/patterns.d/`, next to the vault, honouring
//! `DEVBOY_CONFIG_DIR` like everything else. Opt-in: a missing
//! directory is the normal case and not a warning.
//!
//! # A malformed file
//!
//! Never fatal. The hottest caller is the scrubber that redacts
//! secrets out of text on its way to an agent, and refusing to
//! scrub because a pattern file has a typo would turn a cosmetic
//! mistake into a disclosure. So a load failure falls back to the
//! built-ins — and says so, loudly and once, naming the file and
//! the reason. A silent fallback would leave the user believing
//! their pattern is in force.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::user::{Catalogue, USER_PATTERNS_SUBDIR};

/// Where user pattern files are read from.
///
/// `None` when no config directory can be resolved at all, which is
/// the same condition under which the vault has no home either.
pub fn user_pattern_dir() -> Option<PathBuf> {
    devboy_core::config::Config::secrets_dir()
        .ok()
        .map(|dir| dir.join(USER_PATTERNS_SUBDIR))
}

/// Load a catalogue from `dir`, degrading to built-ins on failure.
///
/// Returns the catalogue and, when something went wrong, the
/// sentence to put in front of the user. Separated from [`shared`]
/// so the policy can be tested against a real directory without
/// touching the process-wide cache — a test that populates a
/// `OnceLock` decides the answer for every other test in the binary.
pub fn catalogue_from(dir: &Path) -> (Catalogue, Option<String>) {
    match Catalogue::load(dir) {
        Ok(catalogue) => (catalogue, None),
        Err(e) => (
            Catalogue::builtins_only(),
            Some(format!(
                "could not load user secret patterns from {}: {e}. Continuing with the built-in \
                 patterns only — any pattern you defined there is NOT in force",
                dir.display()
            )),
        ),
    }
}

/// The catalogue this process uses, loaded once.
///
/// Cached because the response scrubber builds a matcher on every
/// proxied tool result, and re-reading a directory of TOML there
/// would be a file system call per tool call. The cost is that
/// editing a pattern file needs a restart, which is the ordinary
/// bargain for configuration read at startup.
pub fn shared() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let Some(dir) = user_pattern_dir() else {
            return Catalogue::builtins_only();
        };
        let (catalogue, problem) = catalogue_from(&dir);
        if let Some(problem) = problem {
            tracing::warn!("{problem}");
        }
        for warning in catalogue.warnings() {
            tracing::warn!("user secret patterns: {warning}");
        }
        catalogue
    })
}

/// Every pattern this process should match with.
///
/// The shape the scrubber wants, so callers do not each repeat the
/// `.iter()`.
pub fn patterns() -> impl Iterator<Item = &'static dyn crate::SecretPattern> {
    shared().iter().into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed pattern file must not stop the built-ins working.
    ///
    /// The scrubber that redacts secrets out of agent-bound text is
    /// the hottest caller. Refusing to scrub because a pattern file
    /// has a typo would turn a cosmetic mistake into a disclosure.
    #[test]
    fn a_broken_file_falls_back_to_builtins_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mine.toml"), "this is not = = toml").unwrap();

        let (catalogue, problem) = catalogue_from(dir.path());

        assert!(
            !catalogue.iter().is_empty(),
            "the built-ins must survive a bad user file"
        );
        let problem = problem.expect("the user has to be told their pattern is not in force");
        assert!(problem.contains("mine.toml"), "{problem}");
        assert!(
            problem.contains("NOT in force"),
            "a silent fallback leaves the user believing it works: {problem}"
        );
    }

    /// The ordinary case: no directory at all.
    #[test]
    fn a_missing_directory_is_not_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let (catalogue, problem) = catalogue_from(&dir.path().join("absent"));

        assert!(problem.is_none(), "opting out is not an error");
        assert!(!catalogue.iter().is_empty());
        assert!(!catalogue.has_user_patterns());
    }

    /// And the case the whole module exists for.
    #[test]
    fn a_valid_user_pattern_is_actually_loaded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("acme.toml"),
            r#"
[[pattern]]
id = "acme-deploy-key"
display_name = "ACME deploy key"
format_regex = "^acme_deploy_[A-Za-z0-9]{20}$"
severity = "high"
"#,
        )
        .unwrap();

        let (catalogue, problem) = catalogue_from(dir.path());
        assert!(problem.is_none(), "{problem:?}");
        assert!(catalogue.has_user_patterns());
        assert!(
            catalogue.find("acme-deploy-key").is_some(),
            "the pattern the user wrote has to be in the catalogue"
        );
    }
}
