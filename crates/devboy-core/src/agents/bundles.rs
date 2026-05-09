//! Skill bundle profiles for `devboy onboard`.
//!
//! A bundle is a TOML file under `bundles/<profile>.toml` (relative to this
//! crate) listing skill ids that should be installed for a given persona
//! (engineer, PM, on-call). The TOML is `include_str!`-ed at build time so
//! bundles ship inside the binary — no extra files to look up at runtime.

use anyhow::{Result, anyhow};
use serde::Deserialize;

const DEV: &str = include_str!("../../bundles/dev.toml");
const PM: &str = include_str!("../../bundles/pm.toml");
const ONCALL: &str = include_str!("../../bundles/oncall.toml");

#[derive(Debug, Clone, Deserialize)]
/// Bundle.
pub struct Bundle {
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Skills.
    pub skills: Vec<String>,
}

/// All known bundle ids, in display order.
pub const PROFILES: &[&str] = &["dev", "pm", "oncall"];

/// Load a bundle by profile id.
pub fn load(profile: &str) -> Result<Bundle> {
    let raw = match profile {
        "dev" => DEV,
        "pm" => PM,
        "oncall" => ONCALL,
        other => {
            return Err(anyhow!(
                "unknown profile: {other} (known: {})",
                PROFILES.join(", ")
            ));
        }
    };
    toml::from_str::<Bundle>(raw).map_err(|e| anyhow!("failed to parse {profile}.toml: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_profiles_load() {
        for p in PROFILES {
            let b = load(p).unwrap_or_else(|e| panic!("{p}: {e}"));
            assert_eq!(b.name, *p);
            assert!(!b.skills.is_empty(), "{p} has no skills");
        }
    }

    #[test]
    fn unknown_profile_errors() {
        assert!(load("ceo").is_err());
    }

    #[test]
    fn dev_includes_analyze_usage() {
        let b = load("dev").unwrap();
        assert!(b.skills.contains(&"analyze-usage".to_string()));
    }

    #[test]
    fn each_profile_has_unique_skill_ids() {
        for p in PROFILES {
            let b = load(p).unwrap();
            let mut deduped = b.skills.clone();
            deduped.sort();
            deduped.dedup();
            assert_eq!(
                deduped.len(),
                b.skills.len(),
                "profile '{p}' has duplicate skills"
            );
        }
    }

    #[test]
    fn each_profile_starts_with_self_bootstrap() {
        for p in PROFILES {
            let b = load(p).unwrap();
            assert!(
                b.skills.iter().any(|s| s == "setup"),
                "profile '{p}' missing setup"
            );
        }
    }

    #[test]
    fn unknown_profile_error_lists_known_profiles() {
        let err = load("ceo").unwrap_err().to_string();
        for p in PROFILES {
            assert!(
                err.contains(p),
                "error message missing profile '{p}': {err}"
            );
        }
    }

    #[test]
    fn pm_profile_emphasises_meeting_skills() {
        let b = load("pm").unwrap();
        assert!(b.skills.iter().any(|s| s.starts_with("meeting-")));
    }

    #[test]
    fn oncall_profile_includes_notify() {
        let b = load("oncall").unwrap();
        assert!(b.skills.iter().any(|s| s == "notify"));
    }
}
