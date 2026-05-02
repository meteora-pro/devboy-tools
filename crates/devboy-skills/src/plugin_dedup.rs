//! Detect when a Claude Code / Codex plugin already provides our skills,
//! so `devboy onboard` can skip re-installing into the same agent dir.
//!
//! ADR-018 §5: when the user runs `/plugin install devboy@meteora-devboy`,
//! Claude Code records the plugin in `~/.claude/settings.json#enabledPlugins`.
//! At that point the plugin's `skills/` directory is already on the agent's
//! search path; another copy of `~/.claude/skills/devboy-*` would just
//! shadow the namespaced versions. We detect the marker and skip the
//! Claude (or Codex) install target entirely.
//!
//! The detection is intentionally lenient about the exact JSON shape:
//! Claude Code's settings format has evolved across releases, and we
//! recurse the value tree looking for the plugin name as either a key
//! or a string value. False positives are inherently safe — the worst
//! case is one extra "skipped" log line.

use std::fs;
use std::path::Path;

/// `true` if `~/.claude/settings.json` enables a plugin whose name or
/// fully-qualified id contains `plugin_name`.
///
/// Returns `false` when the file is missing, unreadable, or contains no
/// `enabledPlugins` section.
pub fn is_claude_plugin_installed(home: &Path, plugin_name: &str) -> bool {
    is_plugin_enabled_in(&home.join(".claude").join("settings.json"), plugin_name)
}

/// `true` if `~/.codex/settings.json` enables a plugin whose name or
/// fully-qualified id contains `plugin_name`.
///
/// Returns `false` when the file is missing, unreadable, or contains no
/// `enabledPlugins` section. Codex CLI's settings schema is younger than
/// Claude Code's; we apply the same lenient recursive scan.
pub fn is_codex_plugin_installed(home: &Path, plugin_name: &str) -> bool {
    is_plugin_enabled_in(&home.join(".codex").join("settings.json"), plugin_name)
}

fn is_plugin_enabled_in(settings_path: &Path, plugin_name: &str) -> bool {
    let bytes = match fs::read(settings_path) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(enabled) = json.get("enabledPlugins") else {
        return false;
    };
    json_contains_plugin(enabled, plugin_name)
}

fn json_contains_plugin(value: &serde_json::Value, plugin_name: &str) -> bool {
    use serde_json::Value;
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| k.contains(plugin_name) || json_contains_plugin(v, plugin_name)),
        Value::Array(arr) => arr.iter().any(|v| json_contains_plugin(v, plugin_name)),
        Value::String(s) => s.contains(plugin_name),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;
    use tempfile::tempdir;

    fn write_settings(home: &Path, agent_dir: &str, body: &str) {
        let dir = home.join(agent_dir);
        stdfs::create_dir_all(&dir).unwrap();
        stdfs::write(dir.join("settings.json"), body).unwrap();
    }

    #[test]
    fn missing_file_returns_false() {
        let home = tempdir().unwrap();
        assert!(!is_claude_plugin_installed(home.path(), "devboy"));
        assert!(!is_codex_plugin_installed(home.path(), "devboy"));
    }

    #[test]
    fn unrelated_settings_returns_false() {
        let home = tempdir().unwrap();
        write_settings(home.path(), ".claude", r#"{"theme":"dark"}"#);
        assert!(!is_claude_plugin_installed(home.path(), "devboy"));
    }

    #[test]
    fn malformed_json_returns_false() {
        let home = tempdir().unwrap();
        write_settings(home.path(), ".claude", "not json {{{");
        assert!(!is_claude_plugin_installed(home.path(), "devboy"));
    }

    #[test]
    fn detects_plugin_as_object_key() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "meteora-devboy": { "devboy": true }
              }
            }"#,
        );
        assert!(is_claude_plugin_installed(home.path(), "devboy"));
    }

    #[test]
    fn detects_plugin_as_qualified_id_key() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "devboy@meteora-devboy": true
              }
            }"#,
        );
        assert!(is_claude_plugin_installed(home.path(), "devboy"));
    }

    #[test]
    fn detects_plugin_in_array_of_strings() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": ["other-plugin", "devboy@meteora-devboy"]
            }"#,
        );
        assert!(is_claude_plugin_installed(home.path(), "devboy"));
    }

    #[test]
    fn does_not_detect_unrelated_plugin() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "linter": { "prettier": true }
              }
            }"#,
        );
        assert!(!is_claude_plugin_installed(home.path(), "devboy"));
    }

    #[test]
    fn codex_settings_independent_of_claude() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".codex",
            r#"{ "enabledPlugins": { "devboy@meteora-devboy": true } }"#,
        );
        assert!(is_codex_plugin_installed(home.path(), "devboy"));
        // Claude file is missing — Claude detector says no.
        assert!(!is_claude_plugin_installed(home.path(), "devboy"));
    }
}
