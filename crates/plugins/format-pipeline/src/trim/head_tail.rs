//! Head+Tail linear trimming — heuristic for trees > 10000 nodes.
//!
//! Keeps a fraction of items from the head (beginning) and tail (end) of the list.
//! Default split: 30% head, 70% tail (errors/results tend to be at the end).
//!
//! Complexity: O(n) — single pass.
//! Best for: pipeline logs, large flat lists.

use crate::tree::TrimNode;

/// Head/tail budget ratio.
const HEAD_RATIO: f64 = 0.30;
const _TAIL_RATIO: f64 = 0.70;

/// Solve trimming by keeping head and tail items.
pub fn solve(tree: &mut TrimNode, budget: usize) {
    let n = tree.children.len();
    if n == 0 {
        return;
    }

    // Calculate how many items we can fit
    // Estimate average item weight
    let total_weight: usize = tree.children.iter().map(|c| c.total_weight()).sum();
    if total_weight == 0 {
        return;
    }

    let avg_weight = total_weight / n;
    let max_items = if avg_weight > 0 {
        budget / avg_weight
    } else {
        n
    };

    if max_items >= n {
        // Everything fits
        return;
    }

    if max_items == 0 {
        // Nothing fits
        for child in &mut tree.children {
            exclude_subtree(child);
        }
        return;
    }

    // Split budget between head and tail
    let head_count = ((max_items as f64 * HEAD_RATIO).ceil() as usize).max(1);
    let tail_count = max_items.saturating_sub(head_count).max(1);
    let head_count = max_items.saturating_sub(tail_count); // Adjust head after tail min

    // Mark middle items as excluded
    let tail_start = n.saturating_sub(tail_count);

    for (i, child) in tree.children.iter_mut().enumerate() {
        let in_head = i < head_count;
        let in_tail = i >= tail_start;

        if !in_head && !in_tail {
            exclude_subtree(child);
        }
    }

    // Verify budget — if still over, trim from tail end
    while tree.total_weight() > budget {
        // Find last included item and exclude it
        if let Some(last_included) = tree.children.iter().rposition(|c| c.included) {
            exclude_subtree(&mut tree.children[last_included]);
        } else {
            break;
        }
    }
}

fn exclude_subtree(node: &mut TrimNode) {
    node.included = false;
    for child in &mut node.children {
        exclude_subtree(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NodeKind;

    fn make_items(n: usize, weight: usize) -> TrimNode {
        let mut root = TrimNode::new(0, NodeKind::Root, 0);
        for i in 0..n {
            let mut node = TrimNode::new(i + 1, NodeKind::Item { index: i }, weight);
            node.value = 1.0;
            root.children.push(node);
        }
        root
    }

    #[test]
    fn test_head_tail_keeps_ends() {
        let mut tree = make_items(100, 10);
        let budget = 200; // Fits ~20 items
        solve(&mut tree, budget);

        let included = tree.included_item_indices();
        assert!(tree.total_weight() <= budget);

        // Should have items from start and end
        assert!(included.contains(&0), "First item should be kept");
        assert!(included.contains(&99), "Last item should be kept");

        // Middle should be excluded
        assert!(!included.contains(&50), "Middle items should be excluded");
    }

    #[test]
    fn test_head_tail_all_fit() {
        let mut tree = make_items(10, 10);
        solve(&mut tree, 1000);
        assert_eq!(tree.included_items_count(), 10);
    }

    #[test]
    fn test_head_tail_nothing_fits() {
        let mut tree = make_items(10, 100);
        solve(&mut tree, 5);
        assert_eq!(tree.included_items_count(), 0);
    }

    #[test]
    fn test_head_tail_respects_budget() {
        let mut tree = make_items(1000, 10);
        let budget = 500;
        solve(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
    }

    #[test]
    fn test_head_tail_ratio() {
        let mut tree = make_items(200, 10);
        let budget = 1000; // ~100 items

        solve(&mut tree, budget);

        let included = tree.included_item_indices();
        let head_items: Vec<_> = included.iter().filter(|&&i| i < 50).collect();
        let tail_items: Vec<_> = included.iter().filter(|&&i| i >= 150).collect();

        // Tail should get more items than head (70/30 split)
        assert!(
            tail_items.len() >= head_items.len(),
            "Tail ({}) should have >= head ({}) items",
            tail_items.len(),
            head_items.len()
        );
    }

    #[test]
    fn test_head_tail_single_item() {
        let mut tree = make_items(1, 10);
        solve(&mut tree, 5);
        // Single item doesn't fit
        assert_eq!(tree.included_items_count(), 0);
    }

    #[test]
    fn test_head_tail_single_item_fits() {
        let mut tree = make_items(1, 10);
        solve(&mut tree, 100);
        assert_eq!(tree.included_items_count(), 1);
    }

    #[test]
    fn test_head_tail_varying_weights() {
        // Items with different weights
        let mut root = TrimNode::new(0, NodeKind::Root, 0);
        for i in 0..20 {
            let mut node = TrimNode::new(i + 1, NodeKind::Item { index: i }, 10 + i * 5);
            node.value = 1.0;
            root.children.push(node);
        }
        let budget = 100;
        solve(&mut root, budget);
        assert!(root.total_weight() <= budget);
    }

    #[test]
    fn test_head_tail_zero_weight_items() {
        let mut tree = make_items(10, 0);
        solve(&mut tree, 100);
        // All zero-weight items fit
        assert_eq!(tree.included_items_count(), 10);
    }
}
