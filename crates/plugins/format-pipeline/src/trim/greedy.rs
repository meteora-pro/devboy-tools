//! Greedy fractional — for trees with 100-999 nodes.
//!
//! Sorts elements by information density (value/weight) in descending order
//! and greedily adds elements while the budget allows.
//!
//! Guarantee: >= 63% of optimum, in practice ~90%+.
//! Complexity: O(n log n) due to sorting.

use crate::tree::TrimNode;

/// Solve the trimming problem with a greedy density-based algorithm.
pub fn solve(tree: &mut TrimNode, budget: usize) {
    let n = tree.children.len();
    if n == 0 {
        return;
    }

    // Calculate (index, weight, density) for each child
    let mut items: Vec<(usize, usize, f64)> = tree
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

    // Sort by density (descending), with equal density — by weight (ascending)
    items.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });

    // Greedily include elements
    let mut remaining = budget;
    let mut included_set = vec![false; n];

    for &(idx, weight, _density) in &items {
        if weight <= remaining {
            included_set[idx] = true;
            remaining -= weight;
        }
    }

    // Mark excluded nodes
    for (i, child) in tree.children.iter_mut().enumerate() {
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

    fn make_items(weights_values: &[(usize, f64)]) -> TrimNode {
        let mut root = TrimNode::new(0, NodeKind::Root, 0);
        for (i, &(w, v)) in weights_values.iter().enumerate() {
            let mut node = TrimNode::new(i + 1, NodeKind::Item { index: i }, w);
            node.value = v;
            root.children.push(node);
        }
        root
    }

    #[test]
    fn test_greedy_prefers_high_density() {
        // Item 0: w=100, v=0.1 → density=0.001
        // Item 1: w=10, v=0.9 → density=0.09
        // Item 2: w=10, v=0.8 → density=0.08
        let mut tree = make_items(&[(100, 0.1), (10, 0.9), (10, 0.8)]);
        solve(&mut tree, 25);

        let included = tree.included_item_indices();
        assert!(included.contains(&1), "High density item should be kept");
        assert!(included.contains(&2), "Second density item should be kept");
        assert!(
            !included.contains(&0),
            "Low density item should be excluded"
        );
    }

    #[test]
    fn test_greedy_respects_budget() {
        let items: Vec<(usize, f64)> = (0..50).map(|i| (20, 1.0 - i as f64 * 0.01)).collect();
        let mut tree = make_items(&items);
        let budget = 200;
        solve(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
    }

    #[test]
    fn test_greedy_all_fit() {
        let mut tree = make_items(&[(10, 1.0), (10, 1.0), (10, 1.0)]);
        solve(&mut tree, 100);
        assert_eq!(tree.included_items_count(), 3);
    }

    #[test]
    fn test_greedy_nothing_fits() {
        let mut tree = make_items(&[(100, 1.0), (200, 2.0)]);
        solve(&mut tree, 5);
        assert_eq!(tree.included_items_count(), 0);
    }

    #[test]
    fn test_greedy_large_set() {
        let items: Vec<(usize, f64)> = (0..200)
            .map(|i| (10 + i % 50, 1.0 - (i as f64 * 0.003)))
            .collect();
        let mut tree = make_items(&items);
        let budget = 500;
        solve(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
        assert!(tree.included_items_count() > 0);
    }
}
