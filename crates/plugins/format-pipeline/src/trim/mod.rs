//! Tree trimming algorithms by budget.
//!
//! Algorithm selection depends on tree size:
//! - < 100 nodes: Tree Knapsack DP (exact optimum)
//! - 100-999: Greedy fractional (>= 63% of optimum, ~90%+ in practice)
//! - 1000-9999: Hierarchical WFQ (proportionally fair)
//! - >= 10000: Head+Tail linear (heuristic for logs)

pub mod greedy;
pub mod head_tail;
pub mod knapsack;
pub mod wfq;

use crate::tree::TrimNode;

/// Trim the tree to fit the budget, automatically selecting the optimal algorithm.
///
/// Budget is the maximum weight (in tokens) of the resulting included subtree.
/// After the call, excluded nodes have `included = false`.
pub fn trim(tree: &mut TrimNode, budget: usize) {
    // If it already fits — do nothing
    if tree.total_weight() <= budget {
        return;
    }

    let count = tree.count_nodes();
    match count {
        0..=99 => knapsack::solve(tree, budget),
        100..=999 => greedy::solve(tree, budget),
        1000..=9999 => wfq::solve(tree, budget),
        _ => head_tail::solve(tree, budget),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NodeKind;

    fn make_tree(item_weights: &[usize], values: &[f64]) -> TrimNode {
        let mut root = TrimNode::new(0, NodeKind::Root, 0);
        for (i, (&w, &v)) in item_weights.iter().zip(values.iter()).enumerate() {
            let mut node = TrimNode::new(i + 1, NodeKind::Item { index: i }, w);
            node.value = v;
            root.children.push(node);
        }
        root
    }

    #[test]
    fn test_trim_no_op_when_fits() {
        let mut tree = make_tree(&[10, 20, 30], &[1.0, 1.0, 1.0]);
        trim(&mut tree, 100);
        assert_eq!(tree.included_items_count(), 3);
    }

    #[test]
    fn test_trim_reduces_weight() {
        let mut tree = make_tree(&[100, 200, 300], &[1.0, 0.5, 0.2]);
        let budget = 250;
        trim(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
        // Should keep at least one element
        assert!(tree.included_items_count() >= 1);
    }

    #[test]
    fn test_trim_prefers_high_value() {
        let mut tree = make_tree(&[100, 100, 100], &[0.1, 0.9, 0.5]);
        trim(&mut tree, 150);
        // Item with value 0.9 (index 1) should be included
        let included = tree.included_item_indices();
        assert!(included.contains(&1), "High-value item should be kept");
    }

    #[test]
    fn test_trim_selects_correct_algorithm() {
        // < 100 nodes → knapsack
        let mut small = make_tree(&[10; 5], &[1.0; 5]);
        trim(&mut small, 30);
        assert!(small.total_weight() <= 30);

        // > 100 nodes → greedy
        let weights: Vec<usize> = (0..120).map(|_| 10).collect();
        let values: Vec<f64> = (0..120).map(|i| 1.0 - i as f64 * 0.005).collect();
        let mut large = make_tree(&weights, &values);
        trim(&mut large, 500);
        assert!(large.total_weight() <= 500);
    }

    #[test]
    fn test_trim_wfq_range() {
        // 1000-9999 nodes → WFQ
        let weights: Vec<usize> = (0..1500).map(|_| 5).collect();
        let values: Vec<f64> = (0..1500).map(|i| 1.0 - i as f64 * 0.0005).collect();
        let mut tree = make_tree(&weights, &values);
        let budget = 2000;
        trim(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
        assert!(tree.included_items_count() > 0);
    }

    #[test]
    fn test_trim_head_tail_range() {
        // >= 10000 nodes → head_tail
        let weights: Vec<usize> = (0..10500).map(|_| 1).collect();
        let values: Vec<f64> = (0..10500).map(|_| 1.0).collect();
        let mut tree = make_tree(&weights, &values);
        let budget = 500;
        trim(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
    }
}
