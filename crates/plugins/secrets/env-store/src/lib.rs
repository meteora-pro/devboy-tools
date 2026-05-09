//! `devboy-secret-env-store` — built-in `SecretSource` for
//! environment variables per [ADR-021] §8.
//!
//! `env-store` is a first-class source for CI, containers, and
//! bare Linux. There's no keychain to fall back to in those
//! environments; treating env-vars as their own source makes the
//! routing explicit and the failure modes clean.
//!
//! # Resolution order
//!
//! `get("team/gitlab/token-deploy")` walks three tiers:
//!
//! 1. **Manifest alias.** If the global index declared
//!    `env_var = "GITLAB_TOKEN_DEPLOY"` for the path, that
//!    variable name is consulted first. Exists for compatibility
//!    with existing CI conventions where variables already have
//!    well-known names that pre-date `devboy-tools`.
//! 2. **Convention name.** `DEVBOY_SECRET__<flat-path>` — slashes
//!    become `__`, dashes become `_`, the whole thing uppercased.
//!    `team/gitlab/token-deploy` →
//!    `DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY`.
//! 3. **`DEVBOY_SECRETS_FILE` loader.** At construction the source
//!    parses a `.env`-style file pointed at by
//!    `DEVBOY_SECRETS_FILE` (or any path supplied to
//!    [`EnvStoreSource::load_file`]) and keeps the values in an
//!    in-memory `HashMap`. Per ADR-021 the spec talks about
//!    "merging into the process environment" — Rust edition 2024
//!    makes that `unsafe`, so we keep the file values in a side
//!    map and consult them as a fallback after the process env.
//!
//! Process env wins over file values; manifest alias wins over the
//! convention.
//!
//! # Capabilities
//!
//! `READ` only. Per ADR-021 §8: "Validation is format-only;
//! liveness is delegated to the higher-level provider check."
//!
//! # Availability
//!
//! Always [`SourceStatus::Available`] — the env-store has nothing
//! to be unavailable about. Lookups for unset variables return
//! `Ok(None)` rather than failing.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use devboy_storage::{
    Capabilities, GetOutcome, RemoteRef, SecretSource, SourceError, SourceStatus,
};
use secrecy::SecretString;
use thiserror::Error;
use tracing::debug;

/// Env var that points at a `.env`-style file with secrets.
/// Honoured by [`EnvStoreSource::with_file_from_env`] / the
/// `DEVBOY_SECRETS_FILE` constant.
pub const DEVBOY_SECRETS_FILE_ENV: &str = "DEVBOY_SECRETS_FILE";

/// Prefix for the convention-derived env var name.
pub const CONVENTION_PREFIX: &str = "DEVBOY_SECRET__";

// =============================================================================
// File-loader errors
// =============================================================================

/// Failure modes for [`EnvStoreSource::load_file`].
#[derive(Debug, Error)]
pub enum FileLoadError {
    /// I/O error reading the file.
    #[error("failed to read {}: {source}", path.display())]
    Read {
        /// Path the loader tried to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Malformed line — `KEY=value` shape required.
    #[error("{path}:{line}: malformed entry, expected KEY=value")]
    BadLine {
        /// Path of the file being parsed.
        path: String,
        /// 1-indexed line number where parsing failed.
        line: usize,
    },
}

// =============================================================================
// Source
// =============================================================================

/// `SecretSource` that reads from process env vars and an
/// optional `.env`-style fallback file.
#[derive(Debug, Clone, Default)]
pub struct EnvStoreSource {
    /// Logical name from `[[source]] name = "..."` in `sources.toml`.
    name: String,
    /// Pre-resolved env-var aliases per ADR-020 path. The router
    /// seeds this from the global index at construction so the
    /// source doesn't have to know about the manifest layer.
    aliases: HashMap<String, String>,
    /// Values loaded from `DEVBOY_SECRETS_FILE` (or an explicit
    /// override). Consulted only after the process environment.
    file_values: HashMap<String, SecretString>,
}

impl EnvStoreSource {
    /// Build a source named `name`. No aliases or file values yet
    /// — the caller wires those via the builder methods.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            aliases: HashMap::new(),
            file_values: HashMap::new(),
        }
    }

    /// Seed the source with manifest-defined aliases. The map key
    /// is the ADR-020 path string (e.g. `"team/gitlab/token-deploy"`),
    /// the value is the env-var name (e.g. `"GITLAB_TOKEN_DEPLOY"`).
    pub fn with_aliases<I, K, V>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in aliases {
            self.aliases.insert(k.into(), v.into());
        }
        self
    }

    /// Replace the in-memory file values directly. Used by tests
    /// and by callers that have already parsed a `.env` file
    /// through their own loader.
    pub fn with_file_values<I, K, V>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in values {
            self.file_values
                .insert(k.into(), SecretString::from(v.into()));
        }
        self
    }

    /// Load a `.env`-style file and merge its entries into the
    /// in-memory `file_values` map. Returns the number of entries
    /// loaded; previously-loaded keys are overwritten.
    pub fn load_file(&mut self, path: &Path) -> Result<usize, FileLoadError> {
        let body = fs::read_to_string(path).map_err(|source| FileLoadError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let path_str = path.display().to_string();
        let mut count = 0;
        for (idx, raw) in body.lines().enumerate() {
            let line = strip_inline_comment(raw.trim());
            if line.is_empty() {
                continue;
            }
            // Allow `export KEY=value` for compatibility with
            // shell-style .env files.
            let payload = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = match payload.split_once('=') {
                Some(kv) => kv,
                None => {
                    return Err(FileLoadError::BadLine {
                        path: path_str,
                        line: idx + 1,
                    });
                }
            };
            let key = key.trim().to_owned();
            if key.is_empty() {
                return Err(FileLoadError::BadLine {
                    path: path_str,
                    line: idx + 1,
                });
            }
            let value = strip_quotes(value.trim()).to_owned();
            self.file_values.insert(key, SecretString::from(value));
            count += 1;
        }
        debug!(path = ?path, loaded = count, "env-store: file loaded");
        Ok(count)
    }

    /// Convenience: load whatever path `DEVBOY_SECRETS_FILE` points
    /// at if the env var is set and the file exists. No-op
    /// otherwise.
    pub fn with_file_from_env(mut self) -> Result<Self, FileLoadError> {
        if let Ok(s) = std::env::var(DEVBOY_SECRETS_FILE_ENV)
            && !s.is_empty()
        {
            let path = PathBuf::from(s);
            if path.exists() {
                self.load_file(&path)?;
            }
        }
        Ok(self)
    }

    /// Borrow the alias map.
    pub fn aliases(&self) -> &HashMap<String, String> {
        &self.aliases
    }

    /// Number of entries loaded from `.env` files. Useful for
    /// `doctor`-style diagnostics.
    pub fn file_value_count(&self) -> usize {
        self.file_values.len()
    }

    /// Look up `var_name` in process env first, then in the
    /// file-loaded fallback. `secrecy::SecretString` clones cheaply
    /// (Arc-backed), so returning the cloned value here doesn't
    /// duplicate the plaintext beyond one extra Arc reference.
    fn lookup_var(&self, var_name: &str) -> Option<SecretString> {
        if let Ok(v) = std::env::var(var_name) {
            return Some(SecretString::from(v));
        }
        self.file_values.get(var_name).cloned()
    }
}

#[async_trait]
impl SecretSource for EnvStoreSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        // ADR-021 §8: env-store is READ-only. Validation is
        // format-only; the trait's default `validate` (which
        // returns `UnsupportedCapability`) is overridden below to
        // accept any non-empty reference.
        Capabilities::READ
    }

    async fn is_available(&self) -> SourceStatus {
        SourceStatus::Available
    }

    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>, SourceError> {
        if reference.is_empty() {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: "reference must be a non-empty path".to_owned(),
            });
        }
        // Tier 1: manifest alias.
        if let Some(var_name) = self.aliases.get(reference)
            && let Some(value) = self.lookup_var(var_name)
        {
            return Ok(Some(GetOutcome {
                value,
                lease_duration: None,
            }));
        }
        // Tier 2: convention name.
        let conv = convention_name(reference);
        if let Some(value) = self.lookup_var(&conv) {
            return Ok(Some(GetOutcome {
                value,
                lease_duration: None,
            }));
        }
        Ok(None)
    }

    async fn list(&self) -> Result<Vec<RemoteRef>, SourceError> {
        // The env-store doesn't enumerate beyond what its caller
        // already knows: aliases + file values. Surface those
        // (with the alias key as the human-readable display) so
        // `doctor` can show an inventory without re-walking the
        // global index.
        let mut out = Vec::with_capacity(self.aliases.len() + self.file_values.len());
        for (path, var_name) in &self.aliases {
            out.push(RemoteRef {
                reference: path.clone(),
                display: Some(format!("{path} (env: {var_name})")),
            });
        }
        for var_name in self.file_values.keys() {
            // Prefix file-only entries with `env:` so the user can
            // tell them apart from alias-routed paths.
            out.push(RemoteRef {
                reference: format!("env:{var_name}"),
                display: Some(format!("file: {var_name}")),
            });
        }
        Ok(out)
    }

    async fn validate(&self, reference: &str) -> Result<(), SourceError> {
        if reference.is_empty() {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: "reference must be a non-empty path".to_owned(),
            });
        }
        Ok(())
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Convert an ADR-020 path to its convention env-var name.
///
/// `team/gitlab/token-deploy` → `DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY`.
pub fn convention_name(path: &str) -> String {
    let mut out = String::with_capacity(CONVENTION_PREFIX.len() + path.len() + 4);
    out.push_str(CONVENTION_PREFIX);
    for ch in path.chars() {
        match ch {
            '/' => out.push_str("__"),
            '-' => out.push('_'),
            other => out.push(other.to_ascii_uppercase()),
        }
    }
    out
}

/// Strip surrounding `"..."` or `'...'` from a value.
fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Strip a `# comment` tail (when not inside a quoted value).
fn strip_inline_comment(line: &str) -> &str {
    if line.starts_with('#') {
        return "";
    }
    // Conservative: only strip a `#` that follows whitespace and
    // is not inside an obvious quoted region. The `.env` format is
    // famously under-specified; we accept the simple cases.
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single
                && !in_double
                && line[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|p| p.is_whitespace()) =>
            {
                return line[..i].trim_end();
            }
            _ => {}
        }
    }
    line
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Pure-data trait surface ----------------------------------

    #[test]
    fn name_returns_constructor_value() {
        let s = EnvStoreSource::new("env-store");
        assert_eq!(s.name(), "env-store");
    }

    #[test]
    fn capabilities_match_adr_021_section_8_env_store() {
        let s = EnvStoreSource::new("env-store");
        let caps = s.capabilities();
        assert!(caps.contains(Capabilities::READ));
        // env-store declares ONLY read.
        assert!(!caps.contains(Capabilities::LIST));
        assert!(!caps.contains(Capabilities::VALIDATE));
        assert!(!caps.contains(Capabilities::WRITE));
        assert!(!caps.contains(Capabilities::ROTATE));
        assert!(!caps.contains(Capabilities::BIOMETRIC_PROMPT));
        assert!(!caps.contains(Capabilities::AUDIT_LOGGED));
    }

    #[test]
    fn requires_credential_returns_none() {
        let s = EnvStoreSource::new("env-store");
        assert!(s.requires_credential().is_none());
    }

    #[tokio::test]
    async fn is_available_is_unconditional() {
        let s = EnvStoreSource::new("env-store");
        assert_eq!(s.is_available().await, SourceStatus::Available);
    }

    // -- Convention name --------------------------------------------

    #[test]
    fn convention_name_uppercases_replaces_slashes_and_dashes() {
        assert_eq!(
            convention_name("team/gitlab/token-deploy"),
            "DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY"
        );
        assert_eq!(
            convention_name("personal/github/pat"),
            "DEVBOY_SECRET__PERSONAL__GITHUB__PAT"
        );
        assert_eq!(
            convention_name("client-acme/jira/api-key"),
            "DEVBOY_SECRET__CLIENT_ACME__JIRA__API_KEY"
        );
    }

    // -- Tier 1: manifest alias -------------------------------------

    #[tokio::test]
    async fn alias_takes_priority_over_convention_when_both_set() {
        use secrecy::ExposeSecret;
        let s = EnvStoreSource::new("env-store")
            .with_aliases([("team/gitlab/token-deploy", "GITLAB_TOKEN_DEPLOY")]);
        temp_env::async_with_vars(
            [
                ("GITLAB_TOKEN_DEPLOY", Some("from-alias")),
                (
                    "DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY",
                    Some("from-convention"),
                ),
            ],
            async {
                let outcome = s.get("team/gitlab/token-deploy").await.unwrap().unwrap();
                assert_eq!(outcome.value.expose_secret(), "from-alias");
            },
        )
        .await;
    }

    // -- Tier 2: convention name ------------------------------------

    #[tokio::test]
    async fn convention_name_used_when_no_alias() {
        use secrecy::ExposeSecret;
        let s = EnvStoreSource::new("env-store");
        temp_env::async_with_vars(
            [(
                "DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY",
                Some("from-convention"),
            )],
            async {
                let outcome = s.get("team/gitlab/token-deploy").await.unwrap().unwrap();
                assert_eq!(outcome.value.expose_secret(), "from-convention");
            },
        )
        .await;
    }

    #[tokio::test]
    async fn missing_var_returns_none_not_error() {
        let s = EnvStoreSource::new("env-store");
        temp_env::async_with_vars(
            [("DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY", None::<&str>)],
            async {
                let outcome = s.get("team/gitlab/token-deploy").await.unwrap();
                assert!(outcome.is_none());
            },
        )
        .await;
    }

    // -- Tier 3: file fallback --------------------------------------

    #[tokio::test]
    async fn file_value_used_when_process_env_unset() {
        use secrecy::ExposeSecret;
        let s = EnvStoreSource::new("env-store")
            .with_file_values([("DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY", "from-file")]);
        temp_env::async_with_vars(
            [("DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY", None::<&str>)],
            async {
                let outcome = s.get("team/gitlab/token-deploy").await.unwrap().unwrap();
                assert_eq!(outcome.value.expose_secret(), "from-file");
            },
        )
        .await;
    }

    #[tokio::test]
    async fn process_env_wins_over_file_value() {
        use secrecy::ExposeSecret;
        let s = EnvStoreSource::new("env-store")
            .with_file_values([("DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY", "from-file")]);
        temp_env::async_with_vars(
            [(
                "DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY",
                Some("from-env"),
            )],
            async {
                let outcome = s.get("team/gitlab/token-deploy").await.unwrap().unwrap();
                assert_eq!(outcome.value.expose_secret(), "from-env");
            },
        )
        .await;
    }

    // -- File loader -----------------------------------------------

    #[test]
    fn load_file_parses_well_formed_dotenv() {
        use secrecy::ExposeSecret;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secrets.env");
        std::fs::write(
            &path,
            r#"
# devboy-tools test secrets
GITLAB_TOKEN_DEPLOY=glpat-fixture
JIRA_API_KEY="quoted value"
SLACK_TOKEN='single quoted'
export EXPORTED_VAR=exported-value
EMPTY_VALUE=

# trailing comment line
"#,
        )
        .unwrap();
        let mut s = EnvStoreSource::new("env-store");
        let count = s.load_file(&path).unwrap();
        assert_eq!(count, 5);
        assert_eq!(s.file_value_count(), 5);

        let v: SecretString = s.file_values.get("GITLAB_TOKEN_DEPLOY").cloned().unwrap();
        assert_eq!(v.expose_secret(), "glpat-fixture");
        let v: SecretString = s.file_values.get("JIRA_API_KEY").cloned().unwrap();
        assert_eq!(v.expose_secret(), "quoted value");
        let v: SecretString = s.file_values.get("SLACK_TOKEN").cloned().unwrap();
        assert_eq!(v.expose_secret(), "single quoted");
        let v: SecretString = s.file_values.get("EXPORTED_VAR").cloned().unwrap();
        assert_eq!(v.expose_secret(), "exported-value");
        let v: SecretString = s.file_values.get("EMPTY_VALUE").cloned().unwrap();
        assert_eq!(v.expose_secret(), "");
    }

    #[test]
    fn load_file_strips_inline_comments() {
        use secrecy::ExposeSecret;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secrets.env");
        std::fs::write(&path, "X=value-with-trailing # this is a comment\n").unwrap();
        let mut s = EnvStoreSource::new("env-store");
        s.load_file(&path).unwrap();
        let v: SecretString = s.file_values.get("X").cloned().unwrap();
        assert_eq!(v.expose_secret(), "value-with-trailing");
    }

    #[test]
    fn load_file_rejects_malformed_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bad.env");
        std::fs::write(&path, "KEY=ok\nNO_EQUALS_SIGN_HERE\n").unwrap();
        let mut s = EnvStoreSource::new("env-store");
        let err = s.load_file(&path).unwrap_err();
        match err {
            FileLoadError::BadLine { line, .. } => assert_eq!(line, 2),
            other => panic!("expected BadLine, got {other:?}"),
        }
    }

    #[test]
    fn load_file_propagates_io_error() {
        let mut s = EnvStoreSource::new("env-store");
        let err = s
            .load_file(Path::new("/totally-nonexistent/devboy/secrets.env"))
            .unwrap_err();
        assert!(matches!(err, FileLoadError::Read { .. }));
    }

    #[tokio::test]
    async fn with_file_from_env_loads_when_var_points_at_existing_file() {
        use secrecy::ExposeSecret;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secrets.env");
        std::fs::write(&path, "FOO=bar\n").unwrap();
        let path_str = path.to_string_lossy().into_owned();

        temp_env::async_with_vars(
            [(DEVBOY_SECRETS_FILE_ENV, Some(path_str.as_str()))],
            async {
                let s = EnvStoreSource::new("env-store")
                    .with_file_from_env()
                    .unwrap();
                assert_eq!(s.file_value_count(), 1);
                let v: SecretString = s.file_values.get("FOO").cloned().unwrap();
                assert_eq!(v.expose_secret(), "bar");
            },
        )
        .await;
    }

    #[tokio::test]
    async fn with_file_from_env_is_noop_when_var_unset() {
        temp_env::async_with_vars([(DEVBOY_SECRETS_FILE_ENV, None::<&str>)], async {
            let s = EnvStoreSource::new("env-store")
                .with_file_from_env()
                .unwrap();
            assert_eq!(s.file_value_count(), 0);
        })
        .await;
    }

    // -- validate / get edge cases ---------------------------------

    #[tokio::test]
    async fn validate_rejects_empty_reference() {
        let s = EnvStoreSource::new("env-store");
        let err = s.validate("").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    #[tokio::test]
    async fn get_rejects_empty_reference() {
        let s = EnvStoreSource::new("env-store");
        let err = s.get("").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    // -- list -------------------------------------------------------

    #[tokio::test]
    async fn list_surfaces_aliases_and_file_values() {
        let s = EnvStoreSource::new("env-store")
            .with_aliases([("team/gitlab/token-deploy", "GITLAB_TOKEN_DEPLOY")])
            .with_file_values([("EXTRA_VAR", "value")]);
        let entries = s.list().await.unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|e| e.reference == "team/gitlab/token-deploy")
        );
        assert!(entries.iter().any(|e| e.reference == "env:EXTRA_VAR"));
    }

    // -- dyn compat -------------------------------------------------

    #[tokio::test]
    async fn dyn_compat_smoke() {
        let s: Box<dyn SecretSource> = Box::new(EnvStoreSource::new("env-store"));
        assert_eq!(s.name(), "env-store");
        assert_eq!(s.capabilities(), Capabilities::READ);
    }
}
