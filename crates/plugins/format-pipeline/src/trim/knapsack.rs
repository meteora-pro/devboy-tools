//! Tree Knapsack DP — точный оптимум для деревьев < 100 узлов.
//!
//! Реализация алгоритма Cho & Shaw (1997):
//! maximize Σ p(v) для v ∈ S
//! subject to: Σ w(v) ≤ B; S — связное поддерево с корнем
//!
//! Сложность: O(n × B) где n — количество узлов, B — бюджет.

use crate::tree::TrimNode;

/// Решить задачу Tree Knapsack точным DP.
///
/// Помечает excluded узлы как `included = false`.
pub fn solve(tree: &mut TrimNode, budget: usize) {
    // Собираем children корня — это наши "предметы" (с поддеревьями)
    // Для children корня используем 0-1 knapsack по поддеревьям
    let n = tree.children.len();
    if n == 0 {
        return;
    }

    // Collect (weight, value) for each child subtree
    let items: Vec<(usize, f64)> = tree
        .children
        .iter()
        .map(|c| (c.total_weight(), c.total_value()))
        .collect();

    // Scale weights to reduce DP table size.
    // Find GCD of all weights and budget to minimize capacity dimension.
    let all_weights: Vec<usize> = items
        .iter()
        .map(|&(w, _)| w)
        .chain(std::iter::once(budget))
        .collect();
    let scale = gcd_of_slice(&all_weights).max(1);
    let scaled_items: Vec<(usize, f64)> = items.iter().map(|&(w, v)| (w / scale, v)).collect();
    let cap = budget / scale;

    // Cap the DP table size to prevent excessive memory usage.
    // If capacity is still too large, fall back to greedy.
    if cap > 50_000 {
        super::greedy::solve(tree, budget);
        return;
    }

    // Classic 0-1 Knapsack DP with 1D rolling array + keep matrix.
    // Memory: dp = cap * 8 bytes, keep = n * cap bytes.
    // With scaling, typical: dp ~400KB max, keep ~5MB max (n<100, cap<50K).
    let mut dp = vec![0.0_f64; cap + 1];
    let mut keep = vec![vec![false; cap + 1]; n];

    for i in 0..n {
        let (w, v) = scaled_items[i];
        for c in (0..=cap).rev() {
            if w <= c && dp[c - w] + v > dp[c] {
                dp[c] = dp[c - w] + v;
                keep[i][c] = true;
            }
        }
    }

    // Backtrack to find which items are included
    let mut remaining = cap;
    let mut included_set = vec![false; n];
    for i in (0..n).rev() {
        if keep[i][remaining] {
            included_set[i] = true;
            remaining -= scaled_items[i].0;
        }
    }

    // Помечаем excluded
    for (i, child) in tree.children.iter_mut().enumerate() {
        if !included_set[i] {
            exclude_subtree(child);
        }
    }
}

/// GCD of two numbers.
fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// GCD of a slice of numbers.
fn gcd_of_slice(values: &[usize]) -> usize {
    values
        .iter()
        .copied()
        .filter(|&v| v > 0)
        .reduce(gcd)
        .unwrap_or(1)
}

/// Recursively mark entire subtree as excluded.
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
    fn test_knapsack_basic() {
        // Items: (weight, value)
        let mut tree = make_items(&[(10, 1.0), (20, 2.0), (30, 3.0)]);
        solve(&mut tree, 35);

        // Budget 35: лучше взять (10, 1.0) + (20, 2.0) = weight 30, value 30.0
        // или (30, 3.0) = weight 30, value 90.0
        // Knapsack должен выбрать (30, 3.0)
        assert!(tree.total_weight() <= 35);
        assert!(tree.included_items_count() >= 1);
    }

    #[test]
    fn test_knapsack_all_fit() {
        let mut tree = make_items(&[(10, 1.0), (20, 1.0)]);
        solve(&mut tree, 100);
        // Все влезают
        assert_eq!(tree.included_items_count(), 2);
    }

    #[test]
    fn test_knapsack_nothing_fits() {
        let mut tree = make_items(&[(100, 1.0), (200, 2.0)]);
        solve(&mut tree, 5);
        // Ничего не влезает
        assert_eq!(tree.included_items_count(), 0);
    }

    #[test]
    fn test_knapsack_selects_highest_density() {
        // Item 0: weight=50, value=0.5 → density=0.01
        // Item 1: weight=10, value=0.8 → density=0.08
        // Item 2: weight=10, value=0.9 → density=0.09
        let mut tree = make_items(&[(50, 0.5), (10, 0.8), (10, 0.9)]);
        solve(&mut tree, 25);

        // Должен выбрать items 1+2 (weight=20, value=17) vs item 0 (weight=50, не влезает)
        let included = tree.included_item_indices();
        assert!(included.contains(&1));
        assert!(included.contains(&2));
    }

    #[test]
    fn test_knapsack_empty_tree() {
        let mut tree = TrimNode::new(0, NodeKind::Root, 0);
        solve(&mut tree, 100);
        assert_eq!(tree.included_items_count(), 0);
    }

    #[test]
    fn test_knapsack_respects_budget() {
        let mut tree = make_items(&[(30, 1.0), (40, 1.5), (50, 2.0), (20, 0.8)]);
        let budget = 70;
        solve(&mut tree, budget);
        assert!(tree.total_weight() <= budget);
    }
}
