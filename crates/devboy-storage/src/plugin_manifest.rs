//! Sidecar manifest + plugin discovery for `SecretSource`
//! plugins per [ADR-021] §10.
//!
//! ## On-disk layout
//!
//! Each plugin lives in `~/.devboy/plugins/secrets/`:
//!
//! ```text
//! ~/.devboy/plugins/secrets/
//! ├── devboy-source-doppler.toml      ← sidecar manifest
//! └── devboy-source-doppler           ← executable
//! ```
//!
//! The sidecar manifest declares the executable name, version,
//! checksum (SHA-256 hex), and the env vars the plugin is
//! allowed to read. The host enforces these *before* spawning
//! the binary:
//!
//! - **Checksum verification** prevents a swapped-out plugin
//!   from running silently. The `[checksum]` section pins the
//!   bytes the manifest was authored against.
//! - **Allowed env-var list** is the only env the plugin
//!   inherits. Everything else is scrubbed before exec — a
//!   malicious plugin that tries to read `$AWS_SECRET_KEY` to
//!   exfiltrate it sees an empty env.
//!
//! ## What this module does **not** do
//!
//! Spawn the plugin or wire its stdio to the protocol from
//! `plugin_protocol.rs`. That's the plugin client's job
//! (P15.2). This module is purely declarative loading and
//! verification.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

// =============================================================================
// Sidecar manifest
// =============================================================================

/// Filename pattern: `devboy-source-<name>.toml`.
pub const MANIFEST_PREFIX: &str = "devboy-source-";
pub const MANIFEST_SUFFIX: &str = ".toml";

/// Default plugin discovery directory:
/// `$HOME/.devboy/plugins/secrets/`. Scanned by
/// [`discover_plugins_default`].
pub fn default_discovery_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".devboy").join("plugins").join("secrets"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// Plugin name as the host knows it (e.g. `"doppler"`).
    /// Matches the `<name>` in the manifest filename.
    pub name: String,
    /// Plugin version, advisory only — logged and surfaced in
    /// `doctor` output.
    pub version: String,
    /// Path to the executable, relative to the manifest's
    /// directory unless absolute. The discovery layer
    /// canonicalises it before returning.
    pub executable: PathBuf,
    /// Env vars the plugin is allowed to inherit from the
    /// host. Anything not on this list is stripped.
    #[serde(default)]
    pub allowed_env_vars: Vec<String>,
    /// SHA-256 of the executable bytes, lower-case hex. The
    /// host refuses to spawn if the on-disk checksum doesn't
    /// match.
    pub checksum_sha256: String,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error(
        "manifest filename `{filename}` doesn't follow the `devboy-source-<name>.toml` convention"
    )]
    BadFilename { filename: String },
    #[error("manifest `{path}` declares name `{declared}` but the filename says `{from_filename}`")]
    NameMismatch {
        path: PathBuf,
        declared: String,
        from_filename: String,
    },
    #[error("could not read manifest at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest at {path} is malformed: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("executable `{path}` referenced by manifest does not exist")]
    ExecutableMissing { path: PathBuf },
    #[error("could not read executable bytes at {path}: {source}")]
    ChecksumIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("checksum mismatch for `{path}`: manifest declares {declared}, on-disk is {actual}")]
    ChecksumMismatch {
        path: PathBuf,
        declared: String,
        actual: String,
    },
}

impl PluginManifest {
    /// Parse a manifest from a TOML string. Cross-checks that
    /// the declared `name` matches the filename convention.
    pub fn from_toml_str(body: &str, source_path: &Path) -> Result<Self, ManifestError> {
        let m: PluginManifest = toml::from_str(body).map_err(|e| ManifestError::Parse {
            path: source_path.to_path_buf(),
            source: Box::new(e),
        })?;

        let from_filename = name_from_filename(source_path)?;
        if m.name != from_filename {
            return Err(ManifestError::NameMismatch {
                path: source_path.to_path_buf(),
                declared: m.name,
                from_filename,
            });
        }
        Ok(m)
    }

    /// Load + parse + name-cross-check a manifest from disk.
    pub fn load_from(path: &Path) -> Result<Self, ManifestError> {
        let body = fs::read_to_string(path).map_err(|e| ManifestError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::from_toml_str(&body, path)
    }

    /// Resolve the executable path relative to the manifest's
    /// directory, then verify the checksum on disk. Returns
    /// the canonicalised absolute path or an error.
    pub fn verify_executable(&self, manifest_dir: &Path) -> Result<PathBuf, ManifestError> {
        let abs_exec = if self.executable.is_absolute() {
            self.executable.clone()
        } else {
            manifest_dir.join(&self.executable)
        };
        if !abs_exec.exists() {
            return Err(ManifestError::ExecutableMissing { path: abs_exec });
        }
        let bytes = fs::read(&abs_exec).map_err(|e| ManifestError::ChecksumIo {
            path: abs_exec.clone(),
            source: e,
        })?;
        let actual = sha256_hex(&bytes);
        if !checksum_eq(&actual, &self.checksum_sha256) {
            return Err(ManifestError::ChecksumMismatch {
                path: abs_exec,
                declared: self.checksum_sha256.clone(),
                actual,
            });
        }
        Ok(abs_exec)
    }
}

fn name_from_filename(path: &Path) -> Result<String, ManifestError> {
    let filename =
        path.file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| ManifestError::BadFilename {
                filename: path.display().to_string(),
            })?;
    let stripped = filename
        .strip_prefix(MANIFEST_PREFIX)
        .and_then(|s| s.strip_suffix(MANIFEST_SUFFIX))
        .ok_or_else(|| ManifestError::BadFilename {
            filename: filename.to_owned(),
        })?;
    if stripped.is_empty() {
        return Err(ManifestError::BadFilename {
            filename: filename.to_owned(),
        });
    }
    Ok(stripped.to_owned())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn checksum_eq(actual: &str, declared: &str) -> bool {
    actual.eq_ignore_ascii_case(declared)
}

// =============================================================================
// Discovery
// =============================================================================

/// Plugin that survived discovery — manifest parsed cleanly
/// and the executable matches the declared checksum. Ready to
/// hand to the plugin client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    /// Canonicalised absolute path to the verified executable.
    pub executable_path: PathBuf,
    /// The directory the manifest was loaded from.
    pub manifest_dir: PathBuf,
}

/// Per-manifest outcome. Discovery doesn't bubble the first
/// error — a single bad plugin shouldn't hide the others.
#[derive(Debug)]
pub enum DiscoveryOutcome {
    Ok(DiscoveredPlugin),
    Err {
        manifest_path: PathBuf,
        error: ManifestError,
    },
}

/// Scan `dir` for `devboy-source-*.toml` manifests and load
/// and verify each. Non-matching files are silently ignored.
/// Errors are collected per-manifest in the returned outcomes
/// rather than aborting the whole scan.
pub fn discover_plugins(dir: &Path) -> Vec<DiscoveryOutcome> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !filename.starts_with(MANIFEST_PREFIX) || !filename.ends_with(MANIFEST_SUFFIX) {
            continue;
        }
        let manifest_dir = path.parent().unwrap_or(dir).to_path_buf();
        match PluginManifest::load_from(&path) {
            Ok(manifest) => match manifest.verify_executable(&manifest_dir) {
                Ok(exec) => out.push(DiscoveryOutcome::Ok(DiscoveredPlugin {
                    manifest,
                    executable_path: exec,
                    manifest_dir,
                })),
                Err(e) => out.push(DiscoveryOutcome::Err {
                    manifest_path: path,
                    error: e,
                }),
            },
            Err(e) => out.push(DiscoveryOutcome::Err {
                manifest_path: path,
                error: e,
            }),
        }
    }
    out
}

/// Convenience over [`discover_plugins`] + the platform's
/// default discovery directory. Returns an empty Vec if the
/// directory doesn't exist (no plugins installed).
pub fn discover_plugins_default() -> Vec<DiscoveryOutcome> {
    match default_discovery_dir() {
        Some(dir) => discover_plugins(&dir),
        None => Vec::new(),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_plugin(dir: &Path, name: &str, body: &[u8]) -> (PathBuf, PathBuf) {
        let exec_path = dir.join(format!("devboy-source-{name}"));
        let mut f = File::create(&exec_path).unwrap();
        f.write_all(body).unwrap();
        let checksum = sha256_hex(body);

        let manifest_path = dir.join(format!("devboy-source-{name}.toml"));
        let manifest = format!(
            r#"
name = "{name}"
version = "0.1.0"
executable = "devboy-source-{name}"
allowed_env_vars = ["HOME", "PATH"]
checksum_sha256 = "{checksum}"
"#,
        );
        fs::write(&manifest_path, manifest).unwrap();
        (manifest_path, exec_path)
    }

    // -- Manifest parsing -------------------------------------

    #[test]
    fn manifest_parses_minimal_fields() {
        let dir = TempDir::new().unwrap();
        let (manifest_path, _) = write_plugin(dir.path(), "doppler", b"fake-binary");
        let m = PluginManifest::load_from(&manifest_path).unwrap();
        assert_eq!(m.name, "doppler");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.allowed_env_vars, vec!["HOME", "PATH"]);
    }

    #[test]
    fn manifest_rejects_name_filename_mismatch() {
        let dir = TempDir::new().unwrap();
        // Manifest says name="vault" but filename says "doppler".
        let manifest_path = dir.path().join("devboy-source-doppler.toml");
        fs::write(
            &manifest_path,
            r#"
name = "vault"
version = "0.1.0"
executable = "x"
checksum_sha256 = "00"
"#,
        )
        .unwrap();
        let err = PluginManifest::load_from(&manifest_path).unwrap_err();
        assert!(matches!(err, ManifestError::NameMismatch { .. }));
    }

    #[test]
    fn manifest_rejects_filename_without_proper_prefix() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("not-a-plugin.toml");
        fs::write(
            &p,
            "name=\"x\"\nversion=\"0\"\nexecutable=\"x\"\nchecksum_sha256=\"00\"",
        )
        .unwrap();
        let err = PluginManifest::load_from(&p).unwrap_err();
        assert!(matches!(err, ManifestError::BadFilename { .. }));
    }

    #[test]
    fn manifest_rejects_unknown_fields() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("devboy-source-x.toml");
        fs::write(
            &p,
            r#"
name = "x"
version = "0"
executable = "y"
checksum_sha256 = "00"
unknown_field = true
"#,
        )
        .unwrap();
        let err = PluginManifest::load_from(&p).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }

    // -- Checksum verification --------------------------------

    #[test]
    fn verify_executable_passes_for_matching_checksum() {
        let dir = TempDir::new().unwrap();
        let (manifest_path, exec_path) = write_plugin(dir.path(), "doppler", b"hello-world");
        let m = PluginManifest::load_from(&manifest_path).unwrap();
        let resolved = m.verify_executable(dir.path()).unwrap();
        assert_eq!(resolved, exec_path);
    }

    #[test]
    fn verify_executable_fails_when_bytes_change() {
        let dir = TempDir::new().unwrap();
        let (manifest_path, exec_path) = write_plugin(dir.path(), "doppler", b"hello-world");
        // Tamper with the executable.
        fs::write(&exec_path, b"goodbye-world").unwrap();
        let m = PluginManifest::load_from(&manifest_path).unwrap();
        let err = m.verify_executable(dir.path()).unwrap_err();
        assert!(matches!(err, ManifestError::ChecksumMismatch { .. }));
    }

    #[test]
    fn verify_executable_fails_when_binary_missing() {
        let dir = TempDir::new().unwrap();
        let (manifest_path, exec_path) = write_plugin(dir.path(), "doppler", b"hello-world");
        fs::remove_file(&exec_path).unwrap();
        let m = PluginManifest::load_from(&manifest_path).unwrap();
        let err = m.verify_executable(dir.path()).unwrap_err();
        assert!(matches!(err, ManifestError::ExecutableMissing { .. }));
    }

    #[test]
    fn checksum_comparison_is_case_insensitive() {
        let a = "ABCDEF";
        let b = "abcdef";
        assert!(checksum_eq(a, b));
    }

    // -- Discovery --------------------------------------------

    #[test]
    fn discovery_returns_each_valid_plugin() {
        let dir = TempDir::new().unwrap();
        let _ = write_plugin(dir.path(), "doppler", b"a");
        let _ = write_plugin(dir.path(), "vault", b"b");
        let outcomes = discover_plugins(dir.path());
        let oks: Vec<&DiscoveredPlugin> = outcomes
            .iter()
            .filter_map(|o| match o {
                DiscoveryOutcome::Ok(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(oks.len(), 2);
    }

    #[test]
    fn discovery_isolates_per_manifest_errors() {
        let dir = TempDir::new().unwrap();
        let _ = write_plugin(dir.path(), "good", b"a");
        // Corrupt the bad plugin's executable so checksum
        // verification fails.
        let (_, bad_exec) = write_plugin(dir.path(), "bad", b"a");
        fs::write(&bad_exec, b"tampered").unwrap();
        let outcomes = discover_plugins(dir.path());
        let oks = outcomes
            .iter()
            .filter(|o| matches!(o, DiscoveryOutcome::Ok(_)))
            .count();
        let errs = outcomes
            .iter()
            .filter(|o| matches!(o, DiscoveryOutcome::Err { .. }))
            .count();
        assert_eq!(oks, 1);
        assert_eq!(errs, 1);
    }

    #[test]
    fn discovery_ignores_unrelated_files() {
        let dir = TempDir::new().unwrap();
        let _ = write_plugin(dir.path(), "doppler", b"a");
        fs::write(dir.path().join("README.md"), b"unrelated").unwrap();
        fs::write(dir.path().join("notes.txt"), b"unrelated").unwrap();
        let outcomes = discover_plugins(dir.path());
        assert_eq!(outcomes.len(), 1);
    }

    #[test]
    fn discovery_returns_empty_for_missing_dir() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("nope");
        let outcomes = discover_plugins(&nonexistent);
        assert!(outcomes.is_empty());
    }

    #[test]
    fn default_discovery_dir_resolves_under_dot_devboy() {
        let dir = default_discovery_dir().expect("home dir resolvable in test env");
        let suffix: PathBuf = [".devboy", "plugins", "secrets"].iter().collect();
        assert!(
            dir.ends_with(&suffix),
            "expected default dir to end with {}, got {}",
            suffix.display(),
            dir.display()
        );
    }
}
