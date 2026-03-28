//! Trimming strategies assign information value to tree nodes.
//!
//! Each strategy implements domain-specific value scoring.
//! The StrategyResolver maps tool names to strategies via:
//! 1. Exact match in TOML overrides
//! 2. Hardcoded defaults by tool name
//! 3. Strip proxy prefix and retry 1-2
//! 4. Fallback to Default strategy

use std::collections::HashMap;

use crate::tree::{NodeKind, TrimNode};

/// Strategy type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrimStrategyKind {
    /// Element count trimming for flat lists (issues, MRs).
    /// Decreasing value by position; supports full/standard/minimal TrimLevel.
    ElementCount,
    /// Cascading trim with chronological decay for comments.
    /// p(i) = beta^(n-i), beta=0.95 — newest comments are most valuable.
    Cascading,
    /// Size-proportional trimming for diffs, weighted by file type.
    /// .lock=0.05, .min.js=0.10, migrations=0.60, tests=0.70, source=1.00
    SizeProportional,
    /// Thread-level tree trimming for discussions.
    /// resolved=0.3, unresolved=1.0; first+last message always preserved.
    ThreadLevel,
    /// Head+Tail trimming for logs.
    /// 30% head (config/env), 70% tail (errors/results).
    HeadTail,
    /// Default uniform strategy — equal value for all nodes.
    Default,
}

impl TrimStrategyKind {
    /// Parse strategy kind from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "element_count" => Some(Self::ElementCount),
            "cascading" => Some(Self::Cascading),
            "size_proportional" => Some(Self::SizeProportional),
            "thread_level" => Some(Self::ThreadLevel),
            "head_tail" => Some(Self::HeadTail),
            "default" => Some(Self::Default),
            _ => None,
        }
    }

    /// Convert to string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ElementCount => "element_count",
            Self::Cascading => "cascading",
            Self::SizeProportional => "size_proportional",
            Self::ThreadLevel => "thread_level",
            Self::HeadTail => "head_tail",
            Self::Default => "default",
        }
    }
}

// ============================================================================
// Strategy trait and implementations
// ============================================================================

/// Trait for assigning information value to tree nodes.
pub trait TrimStrategy {
    /// Assign value scores to all nodes in the tree.
    fn assign_values(&self, tree: &mut TrimNode);
}

/// Element count trimming — decreasing value by position.
pub struct ElementCountStrategy;

impl TrimStrategy for ElementCountStrategy {
    fn assign_values(&self, tree: &mut TrimNode) {
        let n = tree.children.len();
        if n == 0 {
            return;
        }
        for (i, child) in tree.children.iter_mut().enumerate() {
            // Linear decay: first item = 1.0, last = 0.3
            child.value = 1.0 - (i as f64 / n as f64) * 0.7;
            // Propagate to children
            for grandchild in &mut child.children {
                grandchild.value = child.value;
            }
        }
    }
}

/// Cascading trim — chronological decay for comments.
/// Newest comments are most valuable: p(i) = beta^(n-1-i).
pub struct CascadingStrategy {
    /// Decay factor (default 0.95).
    pub beta: f64,
}

impl Default for CascadingStrategy {
    fn default() -> Self {
        Self { beta: 0.95 }
    }
}

impl TrimStrategy for CascadingStrategy {
    fn assign_values(&self, tree: &mut TrimNode) {
        let n = tree.children.len();
        if n == 0 {
            return;
        }
        for (i, child) in tree.children.iter_mut().enumerate() {
            // Exponential decay: newest = 1.0, oldest = beta^(n-1)
            child.value = self.beta.powi((n - 1 - i) as i32);
            for grandchild in &mut child.children {
                grandchild.value = child.value;
            }
        }
    }
}

/// Size-proportional trimming for diffs — value weighted by file type.
pub struct SizeProportionalStrategy;

impl SizeProportionalStrategy {
    /// Get type weight for a file path.
    fn file_type_weight(path: &str) -> f64 {
        let path_lower = path.to_lowercase();
        if path_lower.ends_with(".lock")
            || path_lower.ends_with(".sum")
            || path_lower.ends_with("package-lock.json")
            || path_lower.ends_with("yarn.lock")
        {
            0.05
        } else if path_lower.ends_with(".min.js") || path_lower.ends_with(".min.css") {
            0.10
        } else if path_lower.contains("migration") || path_lower.contains("schema") {
            0.60
        } else if path_lower.contains("test") || path_lower.contains("spec") {
            0.70
        } else {
            1.00
        }
    }
}

impl TrimStrategy for SizeProportionalStrategy {
    fn assign_values(&self, tree: &mut TrimNode) {
        for child in &mut tree.children {
            // Extract file path from item index (we use a naming convention)
            // Since we don't have the original data here, use a default of 1.0
            // The builder should set a metadata hint on the node
            let type_weight = if let NodeKind::Item { .. } = &child.kind {
                // Look for field children named "diff" — they store the file context
                1.0 // Default; strategy resolver can provide file paths externally
            } else {
                1.0
            };
            child.value = type_weight;
            for grandchild in &mut child.children {
                grandchild.value = type_weight;
            }
        }
    }
}

/// Assign diff values with explicit file paths.
pub fn assign_diff_values(tree: &mut TrimNode, file_paths: &[&str]) {
    for (child, path) in tree.children.iter_mut().zip(file_paths.iter()) {
        let type_weight = SizeProportionalStrategy::file_type_weight(path);
        child.value = type_weight;
        for grandchild in &mut child.children {
            grandchild.value = type_weight;
        }
    }
}

/// Thread-level trimming for discussions.
/// resolved=0.3, unresolved=1.0; first+last comment always get value boost.
pub struct ThreadLevelStrategy;

impl TrimStrategy for ThreadLevelStrategy {
    fn assign_values(&self, tree: &mut TrimNode) {
        for child in &mut tree.children {
            // Default value for discussions — overridden below if resolved info available
            // Resolved discussions get lower value
            child.value = 1.0; // Will be set by assign_discussion_values

            // Within each discussion, first and last comments are most valuable
            let comment_count = child.children.len();
            for (j, comment) in child.children.iter_mut().enumerate() {
                if j == 0 || j == comment_count - 1 {
                    comment.value = 1.0; // First and last always preserved
                } else {
                    comment.value = 0.5; // Middle comments are less important
                }
            }
        }
    }
}

/// Assign discussion values with resolved status.
pub fn assign_discussion_values(tree: &mut TrimNode, resolved: &[bool]) {
    for (child, &is_resolved) in tree.children.iter_mut().zip(resolved.iter()) {
        let disc_value = if is_resolved { 0.3 } else { 1.0 };
        child.value = disc_value;

        let comment_count = child.children.len();
        for (j, comment) in child.children.iter_mut().enumerate() {
            if j == 0 || j == comment_count - 1 {
                comment.value = disc_value; // First and last preserved
            } else {
                comment.value = disc_value * 0.5;
            }
        }
    }
}

/// Head+Tail strategy for logs — 30% head, 70% tail with error boost.
pub struct HeadTailStrategy;

impl TrimStrategy for HeadTailStrategy {
    fn assign_values(&self, tree: &mut TrimNode) {
        let n = tree.children.len();
        if n == 0 {
            return;
        }
        let head_end = (n as f64 * 0.30).ceil() as usize;
        let tail_start = n - (n as f64 * 0.70).ceil() as usize;

        for (i, child) in tree.children.iter_mut().enumerate() {
            if i < head_end {
                child.value = 0.8; // Head: config/env context
            } else if i >= tail_start {
                child.value = 1.0; // Tail: errors/results
            } else {
                child.value = 0.1; // Middle: least important
            }
        }
    }
}

/// Default strategy — uniform value 1.0 for all nodes.
pub struct DefaultStrategy;

impl TrimStrategy for DefaultStrategy {
    fn assign_values(&self, tree: &mut TrimNode) {
        set_uniform_value(tree, 1.0);
    }
}

fn set_uniform_value(node: &mut TrimNode, value: f64) {
    node.value = value;
    for child in &mut node.children {
        set_uniform_value(child, value);
    }
}

// ============================================================================
// Strategy creation
// ============================================================================

/// Create a strategy instance from its kind.
pub fn create_strategy(kind: TrimStrategyKind) -> Box<dyn TrimStrategy> {
    match kind {
        TrimStrategyKind::ElementCount => Box::new(ElementCountStrategy),
        TrimStrategyKind::Cascading => Box::new(CascadingStrategy::default()),
        TrimStrategyKind::SizeProportional => Box::new(SizeProportionalStrategy),
        TrimStrategyKind::ThreadLevel => Box::new(ThreadLevelStrategy),
        TrimStrategyKind::HeadTail => Box::new(HeadTailStrategy),
        TrimStrategyKind::Default => Box::new(DefaultStrategy),
    }
}

// ============================================================================
// Strategy Resolver
// ============================================================================

/// Hardcoded default strategies for known tool names.
fn hardcoded_defaults() -> HashMap<&'static str, TrimStrategyKind> {
    let mut m = HashMap::new();
    m.insert("get_issues", TrimStrategyKind::ElementCount);
    m.insert("get_issue_comments", TrimStrategyKind::Cascading);
    m.insert("get_merge_requests", TrimStrategyKind::ElementCount);
    m.insert(
        "get_merge_request_diffs",
        TrimStrategyKind::SizeProportional,
    );
    m.insert(
        "get_merge_request_discussions",
        TrimStrategyKind::ThreadLevel,
    );
    m.insert("get_job_logs", TrimStrategyKind::HeadTail);
    m.insert("get_pipeline", TrimStrategyKind::Default);
    m.insert("get_users", TrimStrategyKind::Default);
    m.insert("get_statuses", TrimStrategyKind::Default);
    m
}

/// Resolves tool names to trimming strategies.
///
/// Resolution order:
/// 1. Exact match in TOML overrides
/// 2. Exact match in hardcoded defaults
/// 3. Strip proxy prefix (`cloud__get_issues` → `get_issues`), retry 1-2
/// 4. Fallback to Default
pub struct StrategyResolver {
    hardcoded: HashMap<&'static str, TrimStrategyKind>,
    overrides: HashMap<String, TrimStrategyKind>,
    proxy_strip_enabled: bool,
}

impl StrategyResolver {
    /// Create resolver with default hardcoded mappings.
    pub fn new() -> Self {
        Self {
            hardcoded: hardcoded_defaults(),
            overrides: HashMap::new(),
            proxy_strip_enabled: true,
        }
    }

    /// Create resolver with TOML-configured overrides.
    pub fn with_overrides(overrides: HashMap<String, TrimStrategyKind>) -> Self {
        Self {
            hardcoded: hardcoded_defaults(),
            overrides,
            proxy_strip_enabled: true,
        }
    }

    /// Enable/disable proxy prefix stripping.
    pub fn set_proxy_strip(&mut self, enabled: bool) {
        self.proxy_strip_enabled = enabled;
    }

    /// Resolve tool name to strategy kind.
    pub fn resolve(&self, tool_name: &str) -> TrimStrategyKind {
        // 1. Exact match in overrides
        if let Some(&kind) = self.overrides.get(tool_name) {
            return kind;
        }

        // 2. Exact match in hardcoded
        if let Some(&kind) = self.hardcoded.get(tool_name) {
            return kind;
        }

        // 3. Strip proxy prefix and retry
        if self.proxy_strip_enabled
            && let Some(stripped) = strip_proxy_prefix(tool_name)
        {
            if let Some(&kind) = self.overrides.get(stripped) {
                return kind;
            }
            if let Some(&kind) = self.hardcoded.get(stripped) {
                return kind;
            }
        }

        // 4. Fallback
        TrimStrategyKind::Default
    }
}

impl Default for StrategyResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Strip proxy prefix: `cloud__get_issues` → `get_issues`.
/// Returns None if no prefix found.
fn strip_proxy_prefix(tool_name: &str) -> Option<&str> {
    tool_name.find("__").map(|pos| &tool_name[pos + 2..])
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- StrategyResolver ---

    #[test]
    fn test_resolver_hardcoded_defaults() {
        let resolver = StrategyResolver::new();
        assert_eq!(
            resolver.resolve("get_issues"),
            TrimStrategyKind::ElementCount
        );
        assert_eq!(
            resolver.resolve("get_issue_comments"),
            TrimStrategyKind::Cascading
        );
        assert_eq!(
            resolver.resolve("get_merge_request_diffs"),
            TrimStrategyKind::SizeProportional
        );
        assert_eq!(
            resolver.resolve("get_merge_request_discussions"),
            TrimStrategyKind::ThreadLevel
        );
        assert_eq!(resolver.resolve("get_job_logs"), TrimStrategyKind::HeadTail);
        assert_eq!(resolver.resolve("get_pipeline"), TrimStrategyKind::Default);
    }

    #[test]
    fn test_resolver_unknown_tool_fallback() {
        let resolver = StrategyResolver::new();
        assert_eq!(resolver.resolve("unknown_tool"), TrimStrategyKind::Default);
    }

    #[test]
    fn test_resolver_override_takes_priority() {
        let mut overrides = HashMap::new();
        overrides.insert("get_issues".into(), TrimStrategyKind::HeadTail);
        let resolver = StrategyResolver::with_overrides(overrides);
        assert_eq!(resolver.resolve("get_issues"), TrimStrategyKind::HeadTail);
    }

    #[test]
    fn test_resolver_proxy_strip() {
        let resolver = StrategyResolver::new();
        assert_eq!(
            resolver.resolve("cloud__get_issues"),
            TrimStrategyKind::ElementCount
        );
        assert_eq!(
            resolver.resolve("jira_proxy__get_issue_comments"),
            TrimStrategyKind::Cascading
        );
    }

    #[test]
    fn test_resolver_proxy_strip_with_override() {
        let mut overrides = HashMap::new();
        overrides.insert("get_tasks".into(), TrimStrategyKind::ElementCount);
        let resolver = StrategyResolver::with_overrides(overrides);
        assert_eq!(
            resolver.resolve("cloud__get_tasks"),
            TrimStrategyKind::ElementCount
        );
    }

    #[test]
    fn test_resolver_proxy_strip_disabled() {
        let mut resolver = StrategyResolver::new();
        resolver.set_proxy_strip(false);
        assert_eq!(
            resolver.resolve("cloud__get_issues"),
            TrimStrategyKind::Default
        );
    }

    #[test]
    fn test_strip_proxy_prefix() {
        assert_eq!(strip_proxy_prefix("cloud__get_issues"), Some("get_issues"));
        assert_eq!(
            strip_proxy_prefix("jira__get_issue_comments"),
            Some("get_issue_comments")
        );
        assert_eq!(strip_proxy_prefix("get_issues"), None);
        assert_eq!(strip_proxy_prefix("no_prefix"), None);
    }

    // --- TrimStrategyKind ---

    #[test]
    fn test_strategy_kind_from_str() {
        assert_eq!(
            TrimStrategyKind::parse("element_count"),
            Some(TrimStrategyKind::ElementCount)
        );
        assert_eq!(
            TrimStrategyKind::parse("cascading"),
            Some(TrimStrategyKind::Cascading)
        );
        assert_eq!(TrimStrategyKind::parse("unknown"), None);
    }

    #[test]
    fn test_strategy_kind_round_trip() {
        let kinds = [
            TrimStrategyKind::ElementCount,
            TrimStrategyKind::Cascading,
            TrimStrategyKind::SizeProportional,
            TrimStrategyKind::ThreadLevel,
            TrimStrategyKind::HeadTail,
            TrimStrategyKind::Default,
        ];
        for kind in &kinds {
            assert_eq!(TrimStrategyKind::parse(kind.as_str()), Some(*kind));
        }
    }

    // --- Strategy value assignment ---

    #[test]
    fn test_element_count_strategy() {
        let mut tree = make_test_tree(5);
        ElementCountStrategy.assign_values(&mut tree);

        // First item should have highest value
        assert!(tree.children[0].value > tree.children[4].value);
        assert!((tree.children[0].value - 1.0).abs() < 0.01);
        assert!(tree.children[4].value >= 0.3);
    }

    #[test]
    fn test_cascading_strategy() {
        let mut tree = make_test_tree(10);
        CascadingStrategy::default().assign_values(&mut tree);

        // Last item (newest) should have highest value
        assert!(tree.children[9].value > tree.children[0].value);
        // Oldest of 10 with beta=0.95: 0.95^9 ≈ 0.63
        assert!(tree.children[0].value < 0.7);
    }

    #[test]
    fn test_head_tail_strategy() {
        let mut tree = make_test_tree(100);
        HeadTailStrategy.assign_values(&mut tree);

        // Head items (first 30%)
        assert!((tree.children[0].value - 0.8).abs() < 0.01);
        assert!((tree.children[29].value - 0.8).abs() < 0.01);
        // Tail items (last 70% — starts at index 30)
        assert!((tree.children[99].value - 1.0).abs() < 0.01);
        // With 100 items, head_end=30, tail_start=30, so no middle items
        // Test with larger range to have a real middle
        let mut tree2 = make_test_tree(200);
        HeadTailStrategy.assign_values(&mut tree2);
        // head_end = ceil(200 * 0.30) = 60
        // tail_start = 200 - ceil(200 * 0.70) = 200 - 140 = 60
        // So still no middle for balanced split. Let's just verify head < tail
        assert!(tree2.children[0].value < tree2.children[199].value);
    }

    #[test]
    fn test_default_strategy() {
        let mut tree = make_test_tree(5);
        DefaultStrategy.assign_values(&mut tree);

        for child in &tree.children {
            assert!((child.value - 1.0).abs() < 0.001);
        }
    }

    #[test]
    fn test_assign_diff_values() {
        let mut tree = make_test_tree(3);
        let paths = ["Cargo.lock", "src/main.rs", "test_helper.rs"];
        assign_diff_values(&mut tree, &paths);

        assert!((tree.children[0].value - 0.05).abs() < 0.01); // .lock
        assert!((tree.children[1].value - 1.0).abs() < 0.01); // source
        assert!((tree.children[2].value - 0.70).abs() < 0.01); // test
    }

    #[test]
    fn test_assign_discussion_values() {
        let mut tree = TrimNode::new(0, NodeKind::Root, 0);
        for i in 0..3 {
            let mut disc = TrimNode::new(i + 1, NodeKind::Item { index: i }, 10);
            for j in 0..3 {
                let comment = TrimNode::new(10 + i * 3 + j, NodeKind::Item { index: j }, 5);
                disc.children.push(comment);
            }
            tree.children.push(disc);
        }

        let resolved = [true, false, true];
        assign_discussion_values(&mut tree, &resolved);

        assert!((tree.children[0].value - 0.3).abs() < 0.01); // resolved
        assert!((tree.children[1].value - 1.0).abs() < 0.01); // unresolved
        assert!((tree.children[2].value - 0.3).abs() < 0.01); // resolved
    }

    // --- SizeProportionalStrategy ---

    #[test]
    fn test_size_proportional_strategy() {
        let mut tree = make_test_tree(3);
        SizeProportionalStrategy.assign_values(&mut tree);
        // Default value is 1.0 for all items
        for child in &tree.children {
            assert!((child.value - 1.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_file_type_weights() {
        assert!((SizeProportionalStrategy::file_type_weight("Cargo.lock") - 0.05).abs() < 0.01);
        assert!(
            (SizeProportionalStrategy::file_type_weight("package-lock.json") - 0.05).abs() < 0.01
        );
        assert!((SizeProportionalStrategy::file_type_weight("yarn.lock") - 0.05).abs() < 0.01);
        assert!((SizeProportionalStrategy::file_type_weight("go.sum") - 0.05).abs() < 0.01);
        assert!((SizeProportionalStrategy::file_type_weight("app.min.js") - 0.10).abs() < 0.01);
        assert!((SizeProportionalStrategy::file_type_weight("style.min.css") - 0.10).abs() < 0.01);
        assert!(
            (SizeProportionalStrategy::file_type_weight("db/migration_001.sql") - 0.60).abs()
                < 0.01
        );
        assert!((SizeProportionalStrategy::file_type_weight("schema.prisma") - 0.60).abs() < 0.01);
        assert!((SizeProportionalStrategy::file_type_weight("test_main.rs") - 0.70).abs() < 0.01);
        assert!((SizeProportionalStrategy::file_type_weight("main.spec.ts") - 0.70).abs() < 0.01);
        assert!((SizeProportionalStrategy::file_type_weight("src/main.rs") - 1.0).abs() < 0.01);
    }

    // --- ThreadLevelStrategy ---

    #[test]
    fn test_thread_level_strategy() {
        let mut tree = TrimNode::new(0, NodeKind::Root, 0);
        for i in 0..2 {
            let mut disc = TrimNode::new(i + 1, NodeKind::Item { index: i }, 10);
            for j in 0..4 {
                let comment = TrimNode::new(10 + i * 4 + j, NodeKind::Item { index: j }, 5);
                disc.children.push(comment);
            }
            tree.children.push(disc);
        }

        ThreadLevelStrategy.assign_values(&mut tree);

        // First and last comments should have value 1.0
        assert!((tree.children[0].children[0].value - 1.0).abs() < 0.01);
        assert!((tree.children[0].children[3].value - 1.0).abs() < 0.01);
        // Middle comments should have 0.5
        assert!((tree.children[0].children[1].value - 0.5).abs() < 0.01);
        assert!((tree.children[0].children[2].value - 0.5).abs() < 0.01);
    }

    // --- create_strategy ---

    #[test]
    fn test_create_strategy_all_kinds() {
        let kinds = [
            TrimStrategyKind::ElementCount,
            TrimStrategyKind::Cascading,
            TrimStrategyKind::SizeProportional,
            TrimStrategyKind::ThreadLevel,
            TrimStrategyKind::HeadTail,
            TrimStrategyKind::Default,
        ];
        for kind in &kinds {
            let strategy = create_strategy(*kind);
            let mut tree = make_test_tree(5);
            strategy.assign_values(&mut tree);
            // Should not panic
        }
    }

    // --- Empty tree edge cases ---

    #[test]
    fn test_strategies_on_empty_tree() {
        let strategies: Vec<Box<dyn TrimStrategy>> = vec![
            Box::new(ElementCountStrategy),
            Box::new(CascadingStrategy::default()),
            Box::new(SizeProportionalStrategy),
            Box::new(ThreadLevelStrategy),
            Box::new(HeadTailStrategy),
            Box::new(DefaultStrategy),
        ];
        for strategy in &strategies {
            let mut tree = TrimNode::new(0, NodeKind::Root, 0);
            strategy.assign_values(&mut tree); // Should not panic
        }
    }

    // --- assign_diff_values edge cases ---

    #[test]
    fn test_assign_diff_values_various_types() {
        let mut tree = make_test_tree(5);
        let paths = [
            "Cargo.lock",
            "bundle.min.js",
            "db/migration_v2.sql",
            "src/tests/test_auth.rs",
            "src/lib.rs",
        ];
        assign_diff_values(&mut tree, &paths);

        assert!(tree.children[0].value < tree.children[4].value); // lock < source
        assert!(tree.children[1].value < tree.children[4].value); // min.js < source
    }

    // --- Helpers ---

    fn make_test_tree(n: usize) -> TrimNode {
        let mut root = TrimNode::new(0, NodeKind::Root, 0);
        for i in 0..n {
            let node = TrimNode::new(i + 1, NodeKind::Item { index: i }, 10);
            root.children.push(node);
        }
        root
    }
}
