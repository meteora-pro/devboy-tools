//! Per-location manifest (`.manifest.json`) describing installed skills
//! plus the embedded history of every previously-shipped SHA256.
//!
//! See ADR-014 in `docs/architecture/adr/ADR-014-skills-lifecycle.md`
//! at the repository root for the three-state install / upgrade logic
//! that these types drive.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Result, SkillError};

/// Name of the manifest file inside an install target.
pub const MANIFEST_FILE: &str = ".manifest.json";

/// Current manifest schema version. Bumped whenever the shape changes
/// in a way readers need to know about.
pub const MANIFEST_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Embedded historical hashes (shipped alongside baseline skills)
// ---------------------------------------------------------------------------

/// Embedded history file. Ships in the binary; updated by the release
/// tooling whenever a baseline skill's SKILL.md content changes.
#[derive(RustEmbed)]
#[folder = "../../skills/"]
#[include = "history.json"]
struct HistoryAsset;

/// Entry for one historical shipped revision of a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalVersion {
    /// Integer version the skill had at this point.
    pub version: u32,
    /// SHA256 of the full `SKILL.md` file contents (frontmatter + body)
    /// at this version. Matches how [`classify`] and [`classify_path`]
    /// compute their hashes.
    pub sha256: String,
}

/// Per-skill history record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHistory {
    /// The version shipped in this binary.
    pub current: HistoricalVersion,
    /// Previous shipped versions (chronological). Empty for brand-new
    /// skills.
    #[serde(default)]
    pub history: Vec<HistoricalVersion>,
}

impl SkillHistory {
    /// Return true if the given hash matches the current shipped version.
    pub fn is_current(&self, sha256: &str) -> bool {
        self.current.sha256.eq_ignore_ascii_case(sha256)
    }

    /// Return true if the hash matches any previously-shipped version.
    pub fn is_historical(&self, sha256: &str) -> bool {
        self.history
            .iter()
            .any(|h| h.sha256.eq_ignore_ascii_case(sha256))
    }
}

/// Registry of historical hashes for every baseline skill. Loaded from
/// `skills/history.json` at compile time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HistoricalHashes {
    /// Skill name → history record.
    pub by_skill: BTreeMap<String, SkillHistory>,
}

impl HistoricalHashes {
    /// Load the embedded history shipped with this binary. When the
    /// `history.json` asset is missing (early development before the
    /// first release) an empty registry is returned — callers treat
    /// every on-disk skill as "unknown hash" in that case.
    pub fn load_embedded() -> Result<Self> {
        match HistoryAsset::get("history.json") {
            Some(asset) => {
                let parsed: HistoricalHashes =
                    serde_json::from_slice(&asset.data).map_err(|source| {
                        SkillError::InvalidManifest {
                            path: PathBuf::from("<embedded>/history.json"),
                            source,
                        }
                    })?;
                Ok(parsed)
            }
            None => Ok(Self::default()),
        }
    }

    /// Look up the history for a skill name.
    pub fn get(&self, name: &str) -> Option<&SkillHistory> {
        self.by_skill.get(name)
    }
}

// ---------------------------------------------------------------------------
// Per-location install manifest
// ---------------------------------------------------------------------------

/// Recorded file within an installed skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledFile {
    /// SHA256 of the file at install time.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// One installed skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    /// Frontmatter version at install time.
    pub version: u32,
    /// When this skill was installed / last upgraded.
    pub installed_at: DateTime<Utc>,
    /// Name of the source the skill came from.
    pub source: String,
    /// Per-file record (keyed by filename within the skill directory).
    pub files: BTreeMap<String, InstalledFile>,
}

/// Per-location manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version.
    pub version: u32,
    /// Human-readable origin tag (`"devboy-tools 0.18.0"`), for
    /// diagnostics.
    #[serde(default)]
    pub installed_from: Option<String>,
    /// Skill name → record.
    #[serde(default)]
    pub skills: BTreeMap<String, InstalledSkill>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            installed_from: None,
            skills: BTreeMap::new(),
        }
    }
}

impl Manifest {
    /// Load a manifest from disk. Missing files produce an empty
    /// manifest (cold install); corrupt manifests produce an error so
    /// the caller can decide whether to reconstruct.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let parsed: Manifest = serde_json::from_slice(&bytes).map_err(|source| {
                    SkillError::InvalidManifest {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
                Ok(parsed)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(SkillError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Persist the manifest atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| SkillError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let pretty =
            serde_json::to_vec_pretty(self).map_err(|source| SkillError::InvalidManifest {
                path: path.to_path_buf(),
                source,
            })?;

        let tmp_path = path.with_extension("tmp");
        std::fs::write(&tmp_path, &pretty).map_err(|source| SkillError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        // `std::fs::rename` does not overwrite an existing destination
        // on Windows (it does on POSIX). Remove the destination first so
        // the behaviour is consistent across platforms. A missing
        // destination is fine — the rename creates it.
        if path.exists() {
            std::fs::remove_file(path).map_err(|source| SkillError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        std::fs::rename(&tmp_path, path).map_err(|source| SkillError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Mark a skill as installed (or update its record on upgrade).
    pub fn record(&mut self, name: &str, entry: InstalledSkill) {
        self.skills.insert(name.to_string(), entry);
    }

    /// Remove a skill from the manifest.
    pub fn forget(&mut self, name: &str) -> Option<InstalledSkill> {
        self.skills.remove(name)
    }

    /// Look up the stored record for a skill.
    pub fn get(&self, name: &str) -> Option<&InstalledSkill> {
        self.skills.get(name)
    }
}

// ---------------------------------------------------------------------------
// Three-state hash comparator
// ---------------------------------------------------------------------------

/// Outcome of comparing a file on disk against the embedded history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    /// Hash matches the current shipped version — nothing to do.
    Unchanged,
    /// Hash matches a previously-shipped version — safe to overwrite.
    HistoricalSafe,
    /// Hash is unknown — assumed to be a user modification. Install
    /// should refuse without `--force`.
    UserModified,
    /// The skill is not tracked in the embedded history — we cannot
    /// classify it, treat it as user-modified by default.
    Unknown,
}

/// Classify a SKILL.md body against the embedded history registry.
pub fn classify(history: &HistoricalHashes, name: &str, body: &[u8]) -> InstallState {
    let sha = sha256_hex(body);
    let Some(entry) = history.get(name) else {
        return InstallState::Unknown;
    };
    if entry.is_current(&sha) {
        InstallState::Unchanged
    } else if entry.is_historical(&sha) {
        InstallState::HistoricalSafe
    } else {
        InstallState::UserModified
    }
}

/// Read a file and classify it. Missing files produce `None` (nothing
/// to compare against) — the caller interprets absence as a fresh
/// install.
pub fn classify_path(
    history: &HistoricalHashes,
    name: &str,
    path: &Path,
) -> Result<Option<InstallState>> {
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).map_err(|source| SkillError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(Some(classify(history, name, &buf)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SkillError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Hex-encoded SHA256 of the given byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sha256_is_deterministic() {
        let a = sha256_hex(b"hello");
        let b = sha256_hex(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn manifest_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);

        let mut m = Manifest {
            installed_from: Some("devboy-tools 0.18.0".into()),
            ..Default::default()
        };
        let mut files = BTreeMap::new();
        files.insert(
            "SKILL.md".to_string(),
            InstalledFile {
                sha256: "aa".repeat(32),
                size: 42,
            },
        );
        m.record(
            "setup",
            InstalledSkill {
                version: 1,
                installed_at: Utc::now(),
                source: "embedded".into(),
                files,
            },
        );
        m.save(&path).unwrap();

        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.version, MANIFEST_VERSION);
        assert_eq!(loaded.skills["setup"].version, 1);
    }

    #[test]
    fn manifest_load_missing_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let m = Manifest::load(&path).unwrap();
        assert!(m.skills.is_empty());
    }

    #[test]
    fn classify_unchanged_historical_usermod() {
        let current_body = b"current";
        let older_body = b"older";
        let user_body = b"user-edited";

        let mut history = HistoricalHashes::default();
        history.by_skill.insert(
            "setup".into(),
            SkillHistory {
                current: HistoricalVersion {
                    version: 2,
                    sha256: sha256_hex(current_body),
                },
                history: vec![HistoricalVersion {
                    version: 1,
                    sha256: sha256_hex(older_body),
                }],
            },
        );

        assert_eq!(
            classify(&history, "setup", current_body),
            InstallState::Unchanged
        );
        assert_eq!(
            classify(&history, "setup", older_body),
            InstallState::HistoricalSafe
        );
        assert_eq!(
            classify(&history, "setup", user_body),
            InstallState::UserModified
        );
        assert_eq!(
            classify(&history, "devboy-unknown", user_body),
            InstallState::Unknown
        );
    }

    #[test]
    fn skill_history_is_current_and_is_historical_are_case_insensitive() {
        let hist = SkillHistory {
            current: HistoricalVersion {
                version: 2,
                sha256: "AbCdEf1234567890".repeat(4),
            },
            history: vec![HistoricalVersion {
                version: 1,
                sha256: "11223344".repeat(8),
            }],
        };
        // eq_ignore_ascii_case on both branches.
        assert!(hist.is_current(&"abcdef1234567890".repeat(4)));
        assert!(hist.is_historical(&"11223344".repeat(8).to_uppercase()));
        assert!(!hist.is_current("00".repeat(32).as_str()));
        assert!(!hist.is_historical("00".repeat(32).as_str()));
    }

    #[test]
    fn historical_hashes_load_embedded_returns_parsed_or_empty() {
        // The test must not crash regardless of whether `history.json`
        // has been embedded yet — early in development it's empty, once
        // releases land it fills up. Both shapes are valid.
        let hashes = HistoricalHashes::load_embedded().expect("parses or empty");
        for (name, entry) in &hashes.by_skill {
            assert!(!name.is_empty(), "history keys must be non-empty");
            assert!(!entry.current.sha256.is_empty());
        }
    }

    #[test]
    fn manifest_forget_and_get_round_trip() {
        let mut m = Manifest::default();
        assert!(m.get("ghost").is_none());

        let entry = InstalledSkill {
            version: 3,
            installed_at: Utc::now(),
            source: "embedded".into(),
            files: BTreeMap::new(),
        };
        m.record("setup", entry.clone());
        assert_eq!(m.get("setup").unwrap().version, 3);

        let removed = m.forget("setup").expect("entry removed");
        assert_eq!(removed.version, entry.version);
        assert!(m.forget("setup").is_none());
        assert!(m.get("setup").is_none());
    }

    #[test]
    fn manifest_save_overwrites_existing_destination() {
        // Regression: on Windows `fs::rename` does not overwrite. The
        // manifest's atomic-save path removes the destination first;
        // exercise the overwrite branch so that code path stays covered.
        let dir = tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);

        // First write.
        let m1 = Manifest {
            installed_from: Some("v1".into()),
            ..Default::default()
        };
        m1.save(&path).unwrap();

        // Second write must overwrite, not error.
        let mut m2 = Manifest {
            installed_from: Some("v2".into()),
            ..Default::default()
        };
        m2.record(
            "setup",
            InstalledSkill {
                version: 7,
                installed_at: Utc::now(),
                source: "embedded".into(),
                files: BTreeMap::new(),
            },
        );
        m2.save(&path).unwrap();

        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.installed_from.as_deref(), Some("v2"));
        assert_eq!(loaded.skills["setup"].version, 7);
    }

    #[test]
    fn manifest_load_rejects_corrupt_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        std::fs::write(&path, "{ not json").unwrap();
        let err = Manifest::load(&path).unwrap_err();
        assert!(
            matches!(err, SkillError::InvalidManifest { .. }),
            "expected InvalidManifest, got {err:?}"
        );
    }

    #[test]
    fn classify_path_handles_missing_and_present_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("SKILL.md");

        let mut history = HistoricalHashes::default();
        let body = b"ship";
        history.by_skill.insert(
            "s".into(),
            SkillHistory {
                current: HistoricalVersion {
                    version: 1,
                    sha256: sha256_hex(body),
                },
                history: vec![],
            },
        );

        // Missing file → None.
        assert!(classify_path(&history, "s", &path).unwrap().is_none());

        // Present file with matching body → Unchanged.
        std::fs::write(&path, body).unwrap();
        assert_eq!(
            classify_path(&history, "s", &path).unwrap(),
            Some(InstallState::Unchanged)
        );

        // Present file with unknown body → UserModified.
        std::fs::write(&path, b"drifted").unwrap();
        assert_eq!(
            classify_path(&history, "s", &path).unwrap(),
            Some(InstallState::UserModified)
        );
    }
}
