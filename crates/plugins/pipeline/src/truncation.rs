//! Truncation utilities for limiting output size.
//!
//! Provides smart truncation that:
//! - Preserves meaningful content boundaries (lines, words)
//! - Adds truncation markers
//! - Creates agent hints about hidden content

/// Truncate a string to max_chars, preserving word boundaries.
/// The returned string will be at most max_chars long (including ellipsis).
pub fn truncate_string(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }

    // Account for ellipsis in the limit
    let content_limit = max_chars.saturating_sub(3);
    if content_limit == 0 {
        return "...".to_string();
    }

    let truncated = &s[..content_limit.min(s.len())];

    // Try to break at newline first
    if let Some(pos) = truncated.rfind('\n') {
        if pos > content_limit / 2 {
            return format!("{}...", &s[..pos]);
        }
    }

    // Fall back to word boundary
    if let Some(pos) = truncated.rfind(' ') {
        if pos > content_limit / 2 {
            return format!("{}...", &s[..pos]);
        }
    }

    // Hard truncate if no good boundary found
    format!("{}...", truncated)
}

/// Truncate diff content with context preservation.
///
/// Keeps the beginning and end of the diff to show what changed,
/// hiding the middle if too long.
pub fn truncate_diff(diff: &str, max_chars: usize) -> String {
    if diff.len() <= max_chars {
        return diff.to_string();
    }

    let lines: Vec<&str> = diff.lines().collect();
    if lines.len() <= 10 {
        return truncate_string(diff, max_chars);
    }

    // Keep first 5 and last 5 lines, hide the middle
    let head: String = lines[..5].join("\n");
    let tail: String = lines[lines.len() - 5..].join("\n");
    let hidden_count = lines.len() - 10;

    format!(
        "{}\n\n... [{} lines hidden] ...\n\n{}",
        head, hidden_count, tail
    )
}

/// Configuration for truncation plugin.
#[derive(Debug, Clone)]
pub struct TruncationConfig {
    /// Maximum number of items in a list
    pub max_items: usize,
    /// Maximum characters for the entire output
    pub max_total_chars: usize,
    /// Maximum characters per item (e.g., description, diff)
    pub max_item_chars: usize,
    /// Whether to show truncation indicators
    pub show_indicators: bool,
}

impl Default for TruncationConfig {
    fn default() -> Self {
        Self {
            max_items: 20,
            max_total_chars: 4000,
            max_item_chars: 500,
            show_indicators: true,
        }
    }
}

/// Truncation plugin for limiting output size.
pub struct TruncationPlugin {
    config: TruncationConfig,
}

impl TruncationPlugin {
    /// Create a new truncation plugin with default config.
    pub fn new() -> Self {
        Self {
            config: TruncationConfig::default(),
        }
    }

    /// Create a truncation plugin with custom limits.
    pub fn with_limits(max_items: usize, max_chars: usize) -> Self {
        Self {
            config: TruncationConfig {
                max_items,
                max_total_chars: max_chars,
                ..Default::default()
            },
        }
    }

    /// Create a truncation plugin with custom config.
    pub fn with_config(config: TruncationConfig) -> Self {
        Self { config }
    }

    /// Get the maximum number of items.
    pub fn max_items(&self) -> usize {
        self.config.max_items
    }

    /// Get the maximum total characters.
    pub fn max_total_chars(&self) -> usize {
        self.config.max_total_chars
    }

    /// Get the maximum characters per item.
    pub fn max_item_chars(&self) -> usize {
        self.config.max_item_chars
    }

    /// Truncate a string using the plugin's config.
    pub fn truncate(&self, s: &str) -> String {
        truncate_string(s, self.config.max_total_chars)
    }

    /// Truncate an item's content (e.g., description).
    pub fn truncate_item(&self, s: &str) -> String {
        truncate_string(s, self.config.max_item_chars)
    }

    /// Create a truncation summary for agent hint.
    pub fn create_summary(&self, total: usize, shown: usize, item_type: &str) -> String {
        if shown >= total {
            return String::new();
        }

        let remaining = total - shown;
        format!(
            "📊 Showing {}/{} {}. {} more available. Use `offset={}` and `limit={}` for next page.",
            shown, total, item_type, remaining, shown, self.config.max_items
        )
    }
}

impl Default for TruncationPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_string_short() {
        let s = "Hello, world!";
        assert_eq!(truncate_string(s, 100), s);
    }

    #[test]
    fn test_truncate_string_at_word() {
        let s = "Hello world this is a test";
        let result = truncate_string(s, 15);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 18); // 15 + "..."
    }

    #[test]
    fn test_truncate_string_at_newline() {
        let s = "Line 1\nLine 2\nLine 3\nLine 4";
        let result = truncate_string(s, 15);
        assert!(result.contains("Line 1"));
        assert!(result.contains("[truncated]") || result.ends_with("..."));
    }

    #[test]
    fn test_truncate_diff() {
        let diff = (1..=20)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");

        // Use a smaller limit to trigger truncation
        let result = truncate_diff(&diff, 50);
        assert!(result.contains("Line 1"));
        assert!(result.contains("Line 20"));
        assert!(result.contains("lines hidden"));
    }

    #[test]
    fn test_truncate_diff_short() {
        let diff = "Line 1\nLine 2\nLine 3";
        assert_eq!(truncate_diff(diff, 1000), diff);
    }

    #[test]
    fn test_plugin_create_summary() {
        let plugin = TruncationPlugin::with_limits(10, 1000);
        let summary = plugin.create_summary(25, 10, "issues");

        assert!(summary.contains("10/25"));
        assert!(summary.contains("15 more"));
        assert!(summary.contains("offset=10"));
    }

    #[test]
    fn test_plugin_no_summary_when_all_shown() {
        let plugin = TruncationPlugin::new();
        let summary = plugin.create_summary(5, 5, "issues");
        assert!(summary.is_empty());
    }

    #[test]
    fn test_truncate_string_very_small_limit() {
        let s = "Hello, world!";
        let result = truncate_string(s, 3);
        assert_eq!(result, "...");
    }

    #[test]
    fn test_truncate_string_zero_limit() {
        let s = "Hello, world!";
        let result = truncate_string(s, 0);
        assert_eq!(result, "...");
    }

    #[test]
    fn test_truncate_string_hard_truncate() {
        // String with no spaces or newlines — forces hard truncate
        let s = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_string(s, 10);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 13); // 10 - 3 + 3
    }

    #[test]
    fn test_truncate_diff_few_lines() {
        // <= 10 lines, should use truncate_string
        let diff = "L1\nL2\nL3\nL4\nL5\nL6\nL7\nL8";
        let result = truncate_diff(diff, 10);
        assert!(result.ends_with("...") || result == diff);
    }

    #[test]
    fn test_plugin_with_config() {
        let config = TruncationConfig {
            max_items: 5,
            max_total_chars: 200,
            max_item_chars: 50,
            show_indicators: false,
        };
        let plugin = TruncationPlugin::with_config(config);

        assert_eq!(plugin.max_items(), 5);
        assert_eq!(plugin.max_total_chars(), 200);
        assert_eq!(plugin.max_item_chars(), 50);
    }

    #[test]
    fn test_plugin_with_limits() {
        let plugin = TruncationPlugin::with_limits(15, 2000);

        assert_eq!(plugin.max_items(), 15);
        assert_eq!(plugin.max_total_chars(), 2000);
        assert_eq!(plugin.max_item_chars(), 500); // default
    }

    #[test]
    fn test_plugin_truncate() {
        let plugin = TruncationPlugin::with_limits(10, 20);

        let short = "Hello";
        assert_eq!(plugin.truncate(short), "Hello");

        let long = "This is a much longer string that will be truncated";
        let result = plugin.truncate(&long);
        assert!(result.len() <= 23); // 20 + "..."
    }

    #[test]
    fn test_plugin_truncate_item() {
        let config = TruncationConfig {
            max_item_chars: 10,
            ..Default::default()
        };
        let plugin = TruncationPlugin::with_config(config);

        let long = "This is a long item description";
        let result = plugin.truncate_item(&long);
        assert!(result.len() <= 13); // 10 + "..."
    }

    #[test]
    fn test_plugin_default() {
        let plugin = TruncationPlugin::default();
        assert_eq!(plugin.max_items(), 20);
        assert_eq!(plugin.max_total_chars(), 4000);
    }

    #[test]
    fn test_truncation_config_default() {
        let config = TruncationConfig::default();
        assert_eq!(config.max_items, 20);
        assert_eq!(config.max_total_chars, 4000);
        assert_eq!(config.max_item_chars, 500);
        assert!(config.show_indicators);
    }
}
