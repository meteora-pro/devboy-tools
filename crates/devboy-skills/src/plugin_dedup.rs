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
//! Detection is **exact** — we match the plugin id against three concrete
//! `enabledPlugins` shapes that Claude Code and Codex CLI have produced
//! across their releases. A loose substring scan would be unsafe: a false
//! positive here causes `devboy onboard` to skip skill installation, which
//! would leave the user without skills (the plugin we thought was loaded
//! turns out not to be ours).

use std::fs;
use std::path::Path;

/// A plugin's identity inside a marketplace.
///
/// In Claude Code's user settings, the qualified id is
/// `"<name>@<marketplace>"` (e.g. `devboy@meteora-devboy`). Some agent
/// versions also serialise plugins as `{ "<marketplace>": { "<name>":
/// true } }`. Both shapes are matched exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginId {
    /// Plugin name (matches `plugin.json#name`).
    pub name: &'static str,
    /// Owning marketplace name (matches `marketplace.json#name`).
    pub marketplace: &'static str,
}

/// The DevBoy plugin identity. Used by the onboard dedup logic.
pub const DEVBOY_PLUGIN: PluginId = PluginId {
    name: "devboy",
    marketplace: "meteora-devboy",
};

/// `true` if `~/.claude/settings.json` enables the given plugin.
///
/// Returns `false` when the file is missing, unreadable, contains no
/// `enabledPlugins` section, or lists only other plugins.
pub fn is_claude_plugin_enabled(home: &Path, plugin: &PluginId) -> bool {
    is_plugin_enabled_in(&home.join(".claude").join("settings.json"), plugin)
}

/// `true` if `~/.codex/settings.json` enables the given plugin.
///
/// Codex CLI's settings schema is younger than Claude Code's; we apply
/// the same exact-match rules. Returns `false` for missing or empty files.
pub fn is_codex_plugin_enabled(home: &Path, plugin: &PluginId) -> bool {
    is_plugin_enabled_in(&home.join(".codex").join("settings.json"), plugin)
}

// Backwards-compatible thin wrappers for callers that haven't migrated to
// the typed API yet (e.g. ad-hoc scripts).

/// Convenience wrapper: check by qualified id, defaulting to the
/// `meteora-devboy` marketplace.
#[doc(hidden)]
pub fn is_claude_plugin_installed(home: &Path, plugin_name: &str) -> bool {
    if plugin_name == DEVBOY_PLUGIN.name {
        return is_claude_plugin_enabled(home, &DEVBOY_PLUGIN);
    }
    false
}

/// Convenience wrapper: check by qualified id, defaulting to the
/// `meteora-devboy` marketplace.
#[doc(hidden)]
pub fn is_codex_plugin_installed(home: &Path, plugin_name: &str) -> bool {
    if plugin_name == DEVBOY_PLUGIN.name {
        return is_codex_plugin_enabled(home, &DEVBOY_PLUGIN);
    }
    false
}

fn is_plugin_enabled_in(settings_path: &Path, plugin: &PluginId) -> bool {
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
    enabled_plugins_contains(enabled, plugin)
}

/// Match the plugin against the three known `enabledPlugins` shapes:
///
/// 1. Nested: `{ "<marketplace>": { "<name>": true } }`
/// 2. Qualified key: `{ "<name>@<marketplace>": true }`
/// 3. Qualified array element: `["<name>@<marketplace>"]`
///
/// Anything else (substring matches, partial keys, mismatched
/// marketplace) is rejected.
fn enabled_plugins_contains(value: &serde_json::Value, plugin: &PluginId) -> bool {
    use serde_json::Value;
    let qualified = format!("{}@{}", plugin.name, plugin.marketplace);
    match value {
        Value::Object(map) => {
            // Pattern 1 — nested object.
            if let Some(Value::Object(inner)) = map.get(plugin.marketplace)
                && let Some(v) = inner.get(plugin.name)
                && is_truthy(v)
            {
                return true;
            }
            // Pattern 2 — qualified key.
            if let Some(v) = map.get(&qualified)
                && is_truthy(v)
            {
                return true;
            }
            false
        }
        // Pattern 3 — qualified array element.
        Value::Array(arr) => arr
            .iter()
            .any(|v| matches!(v, Value::String(s) if s == &qualified)),
        _ => false,
    }
}

/// `true`, an object with a non-`false` `enabled` field, etc. — anything
/// the agent would interpret as "this plugin is on".
fn is_truthy(value: &serde_json::Value) -> bool {
    use serde_json::Value;
    match value {
        Value::Bool(b) => *b,
        Value::Object(map) => map
            .get("enabled")
            .map(|v| !matches!(v, Value::Bool(false)))
            .unwrap_or(true),
        // String values like "1", "true" are not standard for this field;
        // be strict and treat them as no.
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
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
        assert!(!is_codex_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn unrelated_settings_returns_false() {
        let home = tempdir().unwrap();
        write_settings(home.path(), ".claude", r#"{"theme":"dark"}"#);
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn malformed_json_returns_false() {
        let home = tempdir().unwrap();
        write_settings(home.path(), ".claude", "not json {{{");
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn pattern1_nested_object_marketplace_then_name() {
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
        assert!(is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn pattern2_qualified_key_at_top_level() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{ "enabledPlugins": { "devboy@meteora-devboy": true } }"#,
        );
        assert!(is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn pattern3_qualified_array_element() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{ "enabledPlugins": ["other", "devboy@meteora-devboy"] }"#,
        );
        assert!(is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    // ----------------------------------------------------------------
    // The most important regression tests — false-positive prevention.
    // A loose substring match would have flagged all of these as our
    // plugin and skipped skill install.
    // ----------------------------------------------------------------

    #[test]
    fn unrelated_plugin_with_devboy_substring_does_not_match() {
        let home = tempdir().unwrap();
        // A plugin called "devboy-helper" from a different marketplace.
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "third-party": { "devboy-helper": true }
              }
            }"#,
        );
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn correct_name_wrong_marketplace_does_not_match() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "fork-marketplace": { "devboy": true }
              }
            }"#,
        );
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn explicitly_disabled_plugin_does_not_match() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "meteora-devboy": { "devboy": false }
              }
            }"#,
        );
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn explicitly_disabled_via_qualified_key_does_not_match() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{ "enabledPlugins": { "devboy@meteora-devboy": false } }"#,
        );
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn enabled_object_with_enabled_field_is_truthy() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "meteora-devboy": {
                  "devboy": { "enabled": true, "version": "0.24.0" }
                }
              }
            }"#,
        );
        assert!(is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn enabled_object_with_enabled_false_is_skipped() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".claude",
            r#"{
              "enabledPlugins": {
                "meteora-devboy": {
                  "devboy": { "enabled": false }
                }
              }
            }"#,
        );
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }

    #[test]
    fn codex_settings_independent_of_claude() {
        let home = tempdir().unwrap();
        write_settings(
            home.path(),
            ".codex",
            r#"{ "enabledPlugins": { "devboy@meteora-devboy": true } }"#,
        );
        assert!(is_codex_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
        // Claude file is missing — Claude detector says no.
        assert!(!is_claude_plugin_enabled(home.path(), &DEVBOY_PLUGIN));
    }
}
