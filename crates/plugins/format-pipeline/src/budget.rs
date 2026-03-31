//! Iterative budget pipeline: encode → check → trim → re-encode → verify.
//!
//! The pipeline ensures output fits within a token budget by iteratively:
//! 1. TOON-encode the full data
//! 2. If fits budget → return as-is
//! 3. Calculate B_trim = budget / compression_ratio × (1 - margin)
//! 4. Build TrimTree → apply strategy → trim to B_trim → re-encode
//! 5. Repeat up to max_iterations, adjusting B_trim each time
//! 6. If still over budget → hard truncate

use crate::strategy::{TrimStrategyKind, create_strategy};
use crate::token_counter::estimate_tokens;
use crate::toon::{self, TrimLevel};
use crate::tree::TrimNode;
use crate::trim;
use crate::truncation;

/// Budget pipeline configuration.
#[derive(Debug, Clone)]
pub struct BudgetConfig {
    /// Maximum token budget for output (default: 8000).
    pub budget_tokens: usize,
    /// Safety margin for token estimation inaccuracy (default: 0.20).
    pub margin: f64,
    /// Maximum trim-encode-verify iterations (default: 3).
    pub max_iterations: usize,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            budget_tokens: 8000,
            margin: 0.20,
            max_iterations: 3,
        }
    }
}

/// Result of budget pipeline processing.
#[derive(Debug, Clone)]
pub struct BudgetResult {
    /// TOON-encoded content that fits within budget.
    pub content: String,
    /// Estimated token count of the content.
    pub tokens: usize,
    /// Whether trimming was applied.
    pub trimmed: bool,
    /// Indices of items that were excluded (overflow).
    pub overflow_indices: Vec<usize>,
    /// Total number of items before trimming.
    pub total_items: usize,
    /// Number of items included after trimming.
    pub included_items: usize,
}

/// Run the iterative budget pipeline on a TrimTree.
///
/// Arguments:
/// - `tree` — mutable TrimTree with pre-assigned values from strategy
/// - `full_encoded` — TOON encoding of the complete data (before trimming)
/// - `config` — budget configuration
///
/// Returns BudgetResult with content fitting within budget.
pub fn run(tree: &mut TrimNode, full_encoded: &str, config: &BudgetConfig) -> BudgetResult {
    let full_tokens = estimate_tokens(full_encoded);
    let total_items = tree.included_items_count();

    // Fast path: fits within budget
    if full_tokens <= config.budget_tokens {
        return BudgetResult {
            content: full_encoded.to_string(),
            tokens: full_tokens,
            trimmed: false,
            overflow_indices: vec![],
            total_items,
            included_items: total_items,
        };
    }

    // Calculate initial compression ratio
    let r = if full_tokens > 0 {
        full_tokens as f64 / tree.total_weight() as f64
    } else {
        1.0
    };

    // Calculate initial trim budget: B_trim = budget / r × (1 - margin)
    let effective_budget = config.budget_tokens as f64 * (1.0 - config.margin);
    let mut b_trim = if r > 0.0 {
        (effective_budget / r) as usize
    } else {
        config.budget_tokens
    };

    // Iterative trim-encode-verify loop
    for _iteration in 0..config.max_iterations {
        // Trim tree to b_trim
        trim::trim(tree, b_trim);

        let included_items = tree.included_items_count();
        let overflow = tree.excluded_item_indices();
        let current_weight = tree.total_weight();

        // Estimate tokens after trim (proportional to weight reduction)
        let estimated_tokens = if tree.total_weight() > 0 {
            (current_weight as f64 * r) as usize
        } else {
            0
        };

        if estimated_tokens <= config.budget_tokens {
            // Likely fits — we'll verify with actual encode in the caller
            return BudgetResult {
                content: String::new(), // Caller will re-encode from included indices
                tokens: estimated_tokens,
                trimmed: true,
                overflow_indices: overflow,
                total_items,
                included_items,
            };
        }

        // Adjust b_trim for next iteration
        if estimated_tokens > 0 {
            b_trim =
                (b_trim as f64 * config.budget_tokens as f64 / estimated_tokens as f64) as usize;
        } else {
            break;
        }
    }

    // Final fallback: hard truncate
    let overflow = tree.excluded_item_indices();
    let included_items = tree.included_items_count();

    BudgetResult {
        content: String::new(),
        tokens: 0,
        trimmed: true,
        overflow_indices: overflow,
        total_items,
        included_items,
    }
}

/// High-level: apply strategy and run budget pipeline on issues.
pub fn process_issues(
    issues: &[devboy_core::Issue],
    strategy_kind: TrimStrategyKind,
    config: &BudgetConfig,
) -> devboy_core::Result<BudgetResult> {
    // Encode full data
    let full_encoded = toon::encode_issues(issues, TrimLevel::Full)?;
    let full_tokens = estimate_tokens(&full_encoded);

    // Fast path
    if full_tokens <= config.budget_tokens {
        return Ok(BudgetResult {
            content: full_encoded,
            tokens: full_tokens,
            trimmed: false,
            overflow_indices: vec![],
            total_items: issues.len(),
            included_items: issues.len(),
        });
    }

    // Build tree and apply strategy
    let mut tree = crate::tree::build_issues_tree(issues);
    let strategy = create_strategy(strategy_kind);
    strategy.assign_values(&mut tree);

    // Run budget pipeline
    let mut result = run(&mut tree, &full_encoded, config);

    // Re-encode with only included items
    if result.trimmed {
        let included_indices = tree.included_item_indices();
        let included_issues: Vec<devboy_core::Issue> = included_indices
            .iter()
            .map(|&i| issues[i].clone())
            .collect();

        // Try progressively smaller trim levels until it fits
        for level in [TrimLevel::Full, TrimLevel::Standard, TrimLevel::Minimal] {
            let encoded = toon::encode_issues(&included_issues, level)?;
            let tokens = estimate_tokens(&encoded);
            if tokens <= config.budget_tokens {
                result.content = encoded;
                result.tokens = tokens;
                return Ok(result);
            }
        }

        // Final hard truncate of minimal encoding
        let encoded = toon::encode_issues(&included_issues, TrimLevel::Minimal)?;
        let max_chars = crate::token_counter::tokens_to_chars(config.budget_tokens);
        result.content = truncation::truncate_string(&encoded, max_chars);
        result.tokens = estimate_tokens(&result.content);
    }

    Ok(result)
}

/// High-level: apply strategy and run budget pipeline on merge requests.
pub fn process_merge_requests(
    mrs: &[devboy_core::MergeRequest],
    strategy_kind: TrimStrategyKind,
    config: &BudgetConfig,
) -> devboy_core::Result<BudgetResult> {
    let full_encoded = toon::encode_merge_requests(mrs, TrimLevel::Full)?;
    let full_tokens = estimate_tokens(&full_encoded);

    if full_tokens <= config.budget_tokens {
        return Ok(BudgetResult {
            content: full_encoded,
            tokens: full_tokens,
            trimmed: false,
            overflow_indices: vec![],
            total_items: mrs.len(),
            included_items: mrs.len(),
        });
    }

    let mut tree = crate::tree::build_merge_requests_tree(mrs);
    let strategy = create_strategy(strategy_kind);
    strategy.assign_values(&mut tree);

    let mut result = run(&mut tree, &full_encoded, config);

    if result.trimmed {
        let included_indices = tree.included_item_indices();
        let included_mrs: Vec<devboy_core::MergeRequest> =
            included_indices.iter().map(|&i| mrs[i].clone()).collect();

        for level in [TrimLevel::Full, TrimLevel::Standard, TrimLevel::Minimal] {
            let encoded = toon::encode_merge_requests(&included_mrs, level)?;
            let tokens = estimate_tokens(&encoded);
            if tokens <= config.budget_tokens {
                result.content = encoded;
                result.tokens = tokens;
                return Ok(result);
            }
        }

        let encoded = toon::encode_merge_requests(&included_mrs, TrimLevel::Minimal)?;
        let max_chars = crate::token_counter::tokens_to_chars(config.budget_tokens);
        result.content = truncation::truncate_string(&encoded, max_chars);
        result.tokens = estimate_tokens(&result.content);
    }

    Ok(result)
}

/// High-level: apply strategy and run budget pipeline on diffs.
pub fn process_diffs(
    diffs: &[devboy_core::FileDiff],
    strategy_kind: TrimStrategyKind,
    config: &BudgetConfig,
) -> devboy_core::Result<BudgetResult> {
    let full_encoded = toon::encode_diffs(diffs)?;
    let full_tokens = estimate_tokens(&full_encoded);

    if full_tokens <= config.budget_tokens {
        return Ok(BudgetResult {
            content: full_encoded,
            tokens: full_tokens,
            trimmed: false,
            overflow_indices: vec![],
            total_items: diffs.len(),
            included_items: diffs.len(),
        });
    }

    let mut tree = crate::tree::build_diffs_tree(diffs);
    let strategy = create_strategy(strategy_kind);
    strategy.assign_values(&mut tree);

    let mut result = run(&mut tree, &full_encoded, config);

    if result.trimmed {
        let included_indices = tree.included_item_indices();
        let included_diffs: Vec<devboy_core::FileDiff> =
            included_indices.iter().map(|&i| diffs[i].clone()).collect();

        let encoded = toon::encode_diffs(&included_diffs)?;
        let tokens = estimate_tokens(&encoded);
        if tokens <= config.budget_tokens {
            result.content = encoded;
            result.tokens = tokens;
        } else {
            let max_chars = crate::token_counter::tokens_to_chars(config.budget_tokens);
            result.content = truncation::truncate_string(&encoded, max_chars);
            result.tokens = estimate_tokens(&result.content);
        }
    }

    Ok(result)
}

/// High-level: apply strategy and run budget pipeline on discussions.
pub fn process_discussions(
    discussions: &[devboy_core::Discussion],
    strategy_kind: TrimStrategyKind,
    config: &BudgetConfig,
) -> devboy_core::Result<BudgetResult> {
    let full_encoded = toon::encode_discussions(discussions)?;
    let full_tokens = estimate_tokens(&full_encoded);

    if full_tokens <= config.budget_tokens {
        return Ok(BudgetResult {
            content: full_encoded,
            tokens: full_tokens,
            trimmed: false,
            overflow_indices: vec![],
            total_items: discussions.len(),
            included_items: discussions.len(),
        });
    }

    let mut tree = crate::tree::build_discussions_tree(discussions);
    let strategy = create_strategy(strategy_kind);
    strategy.assign_values(&mut tree);

    let mut result = run(&mut tree, &full_encoded, config);

    if result.trimmed {
        let included_indices = tree.included_item_indices();
        let included_discussions: Vec<devboy_core::Discussion> = included_indices
            .iter()
            .map(|&i| discussions[i].clone())
            .collect();

        let encoded = toon::encode_discussions(&included_discussions)?;
        let tokens = estimate_tokens(&encoded);
        if tokens <= config.budget_tokens {
            result.content = encoded;
            result.tokens = tokens;
        } else {
            let max_chars = crate::token_counter::tokens_to_chars(config.budget_tokens);
            result.content = truncation::truncate_string(&encoded, max_chars);
            result.tokens = estimate_tokens(&result.content);
        }
    }

    Ok(result)
}

/// High-level: apply strategy and run budget pipeline on comments.
pub fn process_comments(
    comments: &[devboy_core::Comment],
    strategy_kind: TrimStrategyKind,
    config: &BudgetConfig,
) -> devboy_core::Result<BudgetResult> {
    let full_encoded = toon::encode_comments(comments)?;
    let full_tokens = estimate_tokens(&full_encoded);

    if full_tokens <= config.budget_tokens {
        return Ok(BudgetResult {
            content: full_encoded,
            tokens: full_tokens,
            trimmed: false,
            overflow_indices: vec![],
            total_items: comments.len(),
            included_items: comments.len(),
        });
    }

    let mut tree = crate::tree::build_comments_tree(comments);
    let strategy = create_strategy(strategy_kind);
    strategy.assign_values(&mut tree);

    let mut result = run(&mut tree, &full_encoded, config);

    if result.trimmed {
        let included_indices = tree.included_item_indices();
        let included_comments: Vec<devboy_core::Comment> = included_indices
            .iter()
            .map(|&i| comments[i].clone())
            .collect();

        let encoded = toon::encode_comments(&included_comments)?;
        let tokens = estimate_tokens(&encoded);
        if tokens <= config.budget_tokens {
            result.content = encoded;
            result.tokens = tokens;
        } else {
            let max_chars = crate::token_counter::tokens_to_chars(config.budget_tokens);
            result.content = truncation::truncate_string(&encoded, max_chars);
            result.tokens = estimate_tokens(&result.content);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NodeKind;

    fn make_tree(weights: &[usize], values: &[f64]) -> TrimNode {
        let mut root = TrimNode::new(0, NodeKind::Root, 0);
        for (i, (&w, &v)) in weights.iter().zip(values.iter()).enumerate() {
            let mut node = TrimNode::new(i + 1, NodeKind::Item { index: i }, w);
            node.value = v;
            root.children.push(node);
        }
        root
    }

    #[test]
    fn test_budget_fast_path() {
        let mut tree = make_tree(&[10, 10, 10], &[1.0, 1.0, 1.0]);
        let content = "short content";
        let config = BudgetConfig {
            budget_tokens: 100,
            ..Default::default()
        };

        let result = run(&mut tree, content, &config);
        assert!(!result.trimmed);
        assert_eq!(result.content, "short content");
        assert!(result.overflow_indices.is_empty());
    }

    #[test]
    fn test_budget_triggers_trimming() {
        let mut tree = make_tree(&[100, 100, 100], &[1.0, 0.5, 0.2]);
        // Create content that's over budget
        let content = "x".repeat(1000); // ~286 tokens
        let config = BudgetConfig {
            budget_tokens: 50,
            margin: 0.20,
            max_iterations: 3,
        };

        let result = run(&mut tree, &content, &config);
        assert!(result.trimmed);
        assert!(result.included_items < 3);
        assert!(!result.overflow_indices.is_empty());
    }

    #[test]
    fn test_budget_config_defaults() {
        let config = BudgetConfig::default();
        assert_eq!(config.budget_tokens, 8000);
        assert!((config.margin - 0.20).abs() < 0.001);
        assert_eq!(config.max_iterations, 3);
    }

    #[test]
    fn test_budget_result_fields() {
        let mut tree = make_tree(&[50, 50, 50], &[1.0, 0.8, 0.3]);
        let content = "x".repeat(600); // ~171 tokens
        let config = BudgetConfig {
            budget_tokens: 30,
            margin: 0.10,
            max_iterations: 2,
        };

        let result = run(&mut tree, &content, &config);
        assert_eq!(result.total_items, 3);
        assert!(result.included_items <= 3);
        assert_eq!(
            result.included_items + result.overflow_indices.len(),
            result.total_items
        );
    }

    // --- process_issues tests ---

    fn sample_issues(n: usize) -> Vec<devboy_core::Issue> {
        (0..n)
            .map(|i| devboy_core::Issue {
                key: format!("gh#{}", i + 1),
                title: format!("Issue {}", i + 1),
                description: Some(format!(
                    "Description for issue {} with enough text to make it non-trivial for token counting purposes",
                    i + 1
                )),
                state: "open".into(),
                source: "github".into(),
                priority: None,
                labels: vec!["bug".into()],
                author: Some(devboy_core::User {
                    id: format!("{}", i),
                    username: format!("user{}", i),
                    name: None,
                    email: None,
                    avatar_url: None,
                }),
                assignees: vec![],
                url: Some(format!("https://github.com/test/repo/issues/{}", i + 1)),
                created_at: Some("2024-01-01T00:00:00Z".into()),
                updated_at: Some("2024-01-02T00:00:00Z".into()),
                parent: None,
                subtasks: vec![],
            })
            .collect()
    }

    #[test]
    fn test_process_issues_fast_path() {
        let issues = sample_issues(2);
        let config = BudgetConfig {
            budget_tokens: 50000,
            ..Default::default()
        };
        let result = process_issues(&issues, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(!result.trimmed);
        assert!(!result.content.is_empty());
        assert_eq!(result.total_items, 2);
        assert_eq!(result.included_items, 2);
    }

    #[test]
    fn test_process_issues_with_trimming() {
        let issues = sample_issues(20);
        let config = BudgetConfig {
            budget_tokens: 500,
            margin: 0.20,
            max_iterations: 3,
        };
        let result = process_issues(&issues, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(result.trimmed);
        assert!(result.included_items < 20);
        assert!(!result.content.is_empty());
        assert!(result.tokens <= config.budget_tokens);
    }

    #[test]
    fn test_process_issues_with_cascading_strategy() {
        let issues = sample_issues(10);
        let config = BudgetConfig {
            budget_tokens: 300,
            margin: 0.20,
            max_iterations: 3,
        };
        let result = process_issues(&issues, TrimStrategyKind::Cascading, &config).unwrap();
        assert!(result.trimmed);
        assert!(!result.content.is_empty());
    }

    #[test]
    fn test_process_issues_very_small_budget() {
        let issues = sample_issues(10);
        let config = BudgetConfig {
            budget_tokens: 50,
            margin: 0.20,
            max_iterations: 3,
        };
        let result = process_issues(&issues, TrimStrategyKind::Default, &config).unwrap();
        assert!(result.trimmed);
        // Content should be truncated but non-empty
        assert!(!result.content.is_empty());
    }

    #[test]
    fn test_process_issues_empty() {
        let config = BudgetConfig::default();
        let result = process_issues(&[], TrimStrategyKind::Default, &config).unwrap();
        assert!(!result.trimmed);
        assert_eq!(result.total_items, 0);
    }

    // --- process_merge_requests tests ---

    fn sample_merge_requests(n: usize) -> Vec<devboy_core::MergeRequest> {
        (0..n)
            .map(|i| devboy_core::MergeRequest {
                key: format!("mr#{}", i + 1),
                title: format!("Merge Request {}", i + 1),
                description: Some(format!(
                    "Description for MR {} with enough text to make it non-trivial for token counting purposes",
                    i + 1
                )),
                state: "opened".into(),
                source: "gitlab".into(),
                source_branch: format!("feature-{}", i + 1),
                target_branch: "main".into(),
                author: Some(devboy_core::User {
                    id: format!("{}", i),
                    username: format!("user{}", i),
                    name: None,
                    email: None,
                    avatar_url: None,
                }),
                assignees: vec![],
                reviewers: vec![],
                labels: vec!["enhancement".into()],
                draft: false,
                url: Some(format!("https://gitlab.com/test/repo/-/merge_requests/{}", i + 1)),
                created_at: Some("2024-01-01T00:00:00Z".into()),
                updated_at: Some("2024-01-02T00:00:00Z".into()),
            })
            .collect()
    }

    #[test]
    fn test_process_merge_requests_fast_path() {
        let mrs = sample_merge_requests(2);
        let config = BudgetConfig {
            budget_tokens: 50000,
            ..Default::default()
        };
        let result = process_merge_requests(&mrs, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(!result.trimmed);
        assert!(!result.content.is_empty());
        assert_eq!(result.total_items, 2);
        assert_eq!(result.included_items, 2);
    }

    #[test]
    fn test_process_merge_requests_with_trimming() {
        let mrs = sample_merge_requests(20);
        let config = BudgetConfig {
            budget_tokens: 500,
            margin: 0.20,
            max_iterations: 3,
        };
        let result = process_merge_requests(&mrs, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(result.trimmed);
        assert!(result.included_items < 20);
        assert!(!result.content.is_empty());
        assert!(result.tokens <= config.budget_tokens);
    }

    #[test]
    fn test_process_merge_requests_empty() {
        let config = BudgetConfig::default();
        let result = process_merge_requests(&[], TrimStrategyKind::Default, &config).unwrap();
        assert!(!result.trimmed);
        assert_eq!(result.total_items, 0);
    }

    // --- process_diffs tests ---

    fn sample_diffs(n: usize) -> Vec<devboy_core::FileDiff> {
        (0..n)
            .map(|i| devboy_core::FileDiff {
                file_path: format!("src/module_{}/file_{}.rs", i / 3, i + 1),
                old_path: None,
                new_file: i == 0,
                deleted_file: false,
                renamed_file: false,
                diff: format!(
                    "@@ -1,10 +1,15 @@\n-old line {i}\n+new line {i}\n+added context for file {i} with enough diff content to be meaningful",
                    i = i + 1
                ),
                additions: Some(2),
                deletions: Some(1),
            })
            .collect()
    }

    #[test]
    fn test_process_diffs_fast_path() {
        let diffs = sample_diffs(2);
        let config = BudgetConfig {
            budget_tokens: 50000,
            ..Default::default()
        };
        let result = process_diffs(&diffs, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(!result.trimmed);
        assert!(!result.content.is_empty());
        assert_eq!(result.total_items, 2);
        assert_eq!(result.included_items, 2);
    }

    #[test]
    fn test_process_diffs_with_trimming() {
        let diffs = sample_diffs(20);
        let config = BudgetConfig {
            budget_tokens: 200,
            margin: 0.20,
            max_iterations: 3,
        };
        let result = process_diffs(&diffs, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(result.trimmed);
        assert!(result.included_items < 20);
        assert!(!result.content.is_empty());
    }

    #[test]
    fn test_process_diffs_empty() {
        let config = BudgetConfig::default();
        let result = process_diffs(&[], TrimStrategyKind::Default, &config).unwrap();
        assert!(!result.trimmed);
        assert_eq!(result.total_items, 0);
    }

    // --- process_discussions tests ---

    fn sample_discussions(n: usize) -> Vec<devboy_core::Discussion> {
        (0..n)
            .map(|i| devboy_core::Discussion {
                id: format!("disc-{}", i + 1),
                resolved: i % 3 == 0,
                resolved_by: None,
                comments: vec![devboy_core::Comment {
                    id: format!("comment-{}", i + 1),
                    body: format!(
                        "Discussion comment {} with enough body text for token counting purposes in the budget pipeline",
                        i + 1
                    ),
                    author: Some(devboy_core::User {
                        id: format!("{}", i),
                        username: format!("reviewer{}", i),
                        name: None,
                        email: None,
                        avatar_url: None,
                    }),
                    created_at: Some("2024-01-01T00:00:00Z".into()),
                    updated_at: None,
                    position: None,
                }],
                position: None,
            })
            .collect()
    }

    #[test]
    fn test_process_discussions_fast_path() {
        let discussions = sample_discussions(2);
        let config = BudgetConfig {
            budget_tokens: 50000,
            ..Default::default()
        };
        let result =
            process_discussions(&discussions, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(!result.trimmed);
        assert!(!result.content.is_empty());
        assert_eq!(result.total_items, 2);
        assert_eq!(result.included_items, 2);
    }

    #[test]
    fn test_process_discussions_with_trimming() {
        let discussions = sample_discussions(20);
        let config = BudgetConfig {
            budget_tokens: 300,
            margin: 0.20,
            max_iterations: 3,
        };
        let result =
            process_discussions(&discussions, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(result.trimmed);
        assert!(result.included_items < 20);
        assert!(!result.content.is_empty());
    }

    #[test]
    fn test_process_discussions_empty() {
        let config = BudgetConfig::default();
        let result = process_discussions(&[], TrimStrategyKind::Default, &config).unwrap();
        assert!(!result.trimmed);
        assert_eq!(result.total_items, 0);
    }

    // --- process_comments tests ---

    fn sample_comments(n: usize) -> Vec<devboy_core::Comment> {
        (0..n)
            .map(|i| devboy_core::Comment {
                id: format!("c-{}", i + 1),
                body: format!(
                    "Comment {} with enough body text to make it non-trivial for budget pipeline token counting",
                    i + 1
                ),
                author: Some(devboy_core::User {
                    id: format!("{}", i),
                    username: format!("commenter{}", i),
                    name: None,
                    email: None,
                    avatar_url: None,
                }),
                created_at: Some("2024-01-01T00:00:00Z".into()),
                updated_at: Some("2024-01-02T00:00:00Z".into()),
                position: None,
            })
            .collect()
    }

    #[test]
    fn test_process_comments_fast_path() {
        let comments = sample_comments(2);
        let config = BudgetConfig {
            budget_tokens: 50000,
            ..Default::default()
        };
        let result = process_comments(&comments, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(!result.trimmed);
        assert!(!result.content.is_empty());
        assert_eq!(result.total_items, 2);
        assert_eq!(result.included_items, 2);
    }

    #[test]
    fn test_process_comments_with_trimming() {
        let comments = sample_comments(20);
        let config = BudgetConfig {
            budget_tokens: 300,
            margin: 0.20,
            max_iterations: 3,
        };
        let result = process_comments(&comments, TrimStrategyKind::ElementCount, &config).unwrap();
        assert!(result.trimmed);
        assert!(result.included_items < 20);
        assert!(!result.content.is_empty());
    }

    #[test]
    fn test_process_comments_empty() {
        let config = BudgetConfig::default();
        let result = process_comments(&[], TrimStrategyKind::Default, &config).unwrap();
        assert!(!result.trimmed);
        assert_eq!(result.total_items, 0);
    }
}
