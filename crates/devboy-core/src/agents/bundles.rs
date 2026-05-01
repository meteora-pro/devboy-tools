//! Skill bundle profiles for `devboy onboard`.
//!
//! A bundle is a TOML file under `skills/bundles/<profile>.toml` listing
//! skill ids that should be installed for a given persona (engineer, PM,
//! on-call). The TOML is `include_str!`-ed at build time so bundles ship
//! inside the binary — no extra files to look up at runtime.

use anyhow::{Result, anyhow};
use serde::Deserialize;

const DEV: &str = include_str!("../../../../skills/bundles/dev.toml");
const PM: &str = include_str!("../../../../skills/bundles/pm.toml");
const ONCALL: &str = include_str!("../../../../skills/bundles/oncall.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Bundle {
    pub name: String,
    pub description: String,
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
}
