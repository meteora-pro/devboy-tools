//! Hierarchical Weighted Fair Queuing — for trees of 1000-9999 nodes.
//!
//! Distributes budget proportionally to subtree value across top-level children.
//! Each child gets budget_share = (child_value / total_value) * budget.
//! Within each subtree, items are greedily selected by density.
//!
//! Complexity: O(n log n) — dominated by sorting within subtrees.
//! Optimality: proportionally fair allocation.

use crate::tree::TrimNode;

/// Solve trimming with hierarchical WFQ.
pub fn solve(tree: &mut TrimNode, budget: usize) {
    let n = tree.children.len();
    if n == 0 {
        return;
    }

    // Calculate total value across all children
    let total_value: f64 = tree.children.iter().map(|c| c.total_value()).sum();
    if total_value <= 0.0 {
        // All zero value — exclude everything
        for child in &mut tree.children {
            exclude_subtree(child);
        }
        return;
    }

    // Allocate budget proportionally to each child's value share
    let mut allocated: Vec<usize> = tree
        .children
        .iter()
        .map(|c| {
            let share = c.total_value() / total_value;
            (share * budget as f64).floor() as usize
        })
        .collect();

    // Distribute remainder to highest-value children
    let used: usize = allocated.iter().sum();
    let mut remainder = budget.saturating_sub(used);
    if remainder > 0 {
        // Sort indices by value (descending) for remainder distribution
        let mut indices: Vec<usize> = (0..n).collect();
        indices.sort_by(|&a, &b| {
            tree.children[b]
                .total_value()
                .partial_cmp(&tree.children[a].total_value())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for &idx in &indices {
            if remainder == 0 {
                break;
            }
            allocated[idx] += 1;
            remainder -= 1;
        }
    }

    // Check if this is a flat tree (all children are leaf items)
    let all_leaves = tree.children.iter().all(|c| c.children.is_empty());
    if all_leaves {
        // For flat trees, WFQ degrades to greedy — use it directly
        super::greedy::solve(tree, budget);
        return;
    }

    // Apply budget to each child subtree
    for (i, child) in tree.children.iter_mut().enumerate() {
        let child_budget = allocated[i];
        let child_weight = child.total_weight();

        if child_weight <= child_budget {
            // Fits — keep everything
            continue;
        }

        if child_budget == 0 {
            exclude_subtree(child);
            continue;
        }

        if child.children.is_empty() {
            if child.weight > child_budget {
                exclude_subtree(child);
            }
        } else {
            // Has grandchildren — apply greedy within this subtree
            trim_children_greedy(child, child_budget);
        }
    }
}

/// Greedy trim within a subtree's children.
fn trim_children_greedy(parent: &mut TrimNode, budget: usize) {
    let mut remaining = budget.saturating_sub(parent.weight); // Reserve weight for parent node itself

    // Calculate density for each child
    let mut items: Vec<(usize, usize, f64)> = parent
        .children
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let w = c.total_weight();
            let v = c.total_value();
            let density = if w > 0 { v / w as f64 } else { 0.0 };
            (i, w, density)
        })
        .collect();

    items.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });

    let mut included_set = vec![false; parent.children.len()];
    for &(idx, weight, _) in &items {
        if weight <= remaining {
            included_set[idx] = true;
            remaining -= weight;
        }
    }

    for (i, child) in parent.children.iter_mut().enumerate() {
        if !included_set[i] {
            exclude_subtree(child);
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

    fn make_tree_with_subtrees() -> TrimNode {
        let mut root = TrimNode::new(0, NodeKind::Root, 0);

        // Subtree 0: high value (3 items, total weight ~60)
        let mut sub0 = TrimNode::new(1, NodeKind::Item { index: 0 }, 5);
        sub0.value = 1.0;
        for j in 0..3 {
            let mut item = TrimNode::new(10 + j, NodeKind::Item { index: j }, 20);
            item.value = 0.9;
            sub0.children.push(item);
        }

        // Subtree 1: low value (3 items, total weight ~60)
        let mut sub1 = TrimNode::new(2, NodeKind::Item { index: 1 }, 5);
        sub1.value = 0.3;
        for j in 0..3 {
            let mut item = TrimNode::new(20 + j, NodeKind::Item { index: j }, 20);
            item.value = 0.2;
            sub1.children.push(item);
        }

        root.children.push(sub0);
        root.children.push(sub1);
        root
    }

    #[test]
    fn test_wfq_proportional_allocation() {
        let mut tree = make_tree_with_subtrees();
        let budget = 80; // Less than total (~130)
        solve(&mut tree, budget);

        assert!(tree.total_weight() <= budget);
        // High-value subtree should get more budget
        let sub0_weight = tree.children[0].total_weight();
        let sub1_weight = tree.children[1].total_weight();
        assert!(
            sub0_weight >= sub1_weight,
            "High-value subtree ({}) should get more than low-value ({})",
            sub0_weight,
            sub1_weight
        );
    }

    #[test]
    fn test_wfq_respects_budget() {
        let mut tree = make_tree_with_subtrees();
        let budget = 50;
        solve(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
    }

    #[test]
    fn test_wfq_all_fit() {
        let mut tree = make_tree_with_subtrees();
        solve(&mut tree, 10000);
        // Everything should be included
        assert!(tree.children[0].included);
        assert!(tree.children[1].included);
    }

    #[test]
    fn test_wfq_flat_children() {
        // No subtrees — just flat items
        let mut root = TrimNode::new(0, NodeKind::Root, 0);
        for i in 0..10 {
            let mut node = TrimNode::new(i + 1, NodeKind::Item { index: i }, 20);
            node.value = 1.0 - i as f64 * 0.05;
            root.children.push(node);
        }

        let budget = 80;
        solve(&mut root, budget);
        assert!(root.total_weight() <= budget);
        assert!(root.included_items_count() > 0);
    }
}
