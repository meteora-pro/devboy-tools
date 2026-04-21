//! TrimTree strategy ablation eval harness.
//!
//! Measures Priority Hit Rate (p₁) across strategies, dataset types, and token budgets.
//!
//! # Usage
//!
//! ```shell
//! cargo run -p devboy-format-pipeline --bin eval -- --output results.csv
//! cargo run -p devboy-format-pipeline --bin eval -- --trials 500
//! ```
//!
//! # Output CSV
//!
//! ```
//! dataset_type,n_items,budget_tokens,strategy,p1,mean_tokens_used,mean_items_included,trials
//! ```
//!
//! # Empirical calibration
//!
//! Dataset parameters are calibrated from real Claude Code session logs
//! (523 sessions, 10,644 MCP responses via track-claude-usage):
//! - Median item weight ≈ 686 tokens (2400 chars / 3.5 chars·token⁻¹)
//! - Practical budgets: 1k–8k tokens (typical context window allocation)
//! - Gold item distribution: top 30% by recency (mirrors SWE-bench ground truth)
//! - 0% pagination rate observed — agents never request chunk=2 → p₁ is the right metric

use std::env;
use std::io::{self, Write};

use devboy_format_pipeline::strategy::{
    ItemMetadata, TrimStrategyKind, assign_priority_values, create_strategy,
};
use devboy_format_pipeline::tree::{NodeKind, TrimNode};
use devboy_format_pipeline::trim;

// ============================================================================
// Dataset types
// ============================================================================

/// Type of synthetic dataset, each modelling a different real-world distribution.
#[derive(Debug, Clone, Copy)]
enum DatasetType {
    /// All items have equal value. Baseline — strategy shouldn't matter.
    Uniform,
    /// Values follow power-law (α=1.5). Gold in top 20% by value.
    /// Mirrors real GitLab issue priority distributions.
    PowerLaw,
    /// Gold item is the LAST item. Worst case for position-biased strategies.
    Adversarial,
    /// Gold item is uniformly random. Values from truncated normal.
    /// Closest to real Claude Code session patterns.
    Realistic,
}

impl DatasetType {
    fn name(self) -> &'static str {
        match self {
            Self::Uniform => "uniform",
            Self::PowerLaw => "power_law",
            Self::Adversarial => "adversarial",
            Self::Realistic => "realistic",
        }
    }

    fn all() -> &'static [Self] {
        &[Self::Uniform, Self::PowerLaw, Self::Adversarial, Self::Realistic]
    }
}

// ============================================================================
// LCG pseudo-random generator (no external deps)
// ============================================================================

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    /// Returns a float in [0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Returns an integer in [0, n).
    fn next_usize(&mut self, n: usize) -> usize {
        (self.next_f64() * n as f64) as usize
    }
}

// ============================================================================
// Dataset generation
// ============================================================================

/// A generated dataset: N items with weights and a gold_index.
struct Dataset {
    /// Per-item token weights (calibrated to real log data).
    item_weights: Vec<usize>,
    /// Index of the "gold" item that the agent actually needs.
    gold_index: usize,
    /// Optional metadata for Priority strategy.
    metadata: Vec<ItemMetadata>,
}

impl Dataset {
    fn generate(dtype: DatasetType, n: usize, rng: &mut Lcg) -> Self {
        // Item weights: calibrated from real logs
        // Median = 686 tokens, uniform ±30% jitter
        let item_weights: Vec<usize> = (0..n)
            .map(|_| {
                let jitter = 0.7 + rng.next_f64() * 0.6; // [0.7, 1.3]
                (686.0 * jitter) as usize
            })
            .collect();

        let (gold_index, metadata) = match dtype {
            DatasetType::Uniform => {
                let gold = rng.next_usize(n);
                let meta = vec![ItemMetadata::default(); n];
                (gold, meta)
            }

            DatasetType::PowerLaw => {
                // Power-law: value(i) = (n - rank)^α, α=1.5. Ranks assigned randomly.
                let mut ranks: Vec<usize> = (0..n).collect();
                for i in (1..n).rev() {
                    let j = rng.next_usize(i + 1);
                    ranks.swap(i, j);
                }
                let values: Vec<f64> = ranks
                    .iter()
                    .map(|&rank| ((n - rank) as f64).powf(1.5))
                    .collect();
                // Gold: pick from top 20% by value
                let mut sorted_indices: Vec<usize> = (0..n).collect();
                sorted_indices.sort_by(|&a, &b| values[b].partial_cmp(&values[a]).unwrap());
                let top_20pct = (n / 5).max(1);
                let gold = sorted_indices[rng.next_usize(top_20pct)];
                let meta = metadata_from_values(&values, rng);
                (gold, meta)
            }

            DatasetType::Adversarial => {
                // Gold is the LAST item — worst case for position-biased strategies
                let values: Vec<f64> = (0..n)
                    .map(|i| 0.3 + (i as f64 / n as f64) * 0.7)
                    .collect();
                let meta = metadata_from_values(&values, rng);
                (n - 1, meta)
            }

            DatasetType::Realistic => {
                // Truncated normal around 0.5, gold uniformly random
                let values: Vec<f64> = (0..n)
                    .map(|_| {
                        let u = rng.next_f64().max(1e-10);
                        let v = rng.next_f64().max(1e-10);
                        let z = (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos();
                        (0.5 + z * 0.2).clamp(0.01, 1.0)
                    })
                    .collect();
                let gold = rng.next_usize(n);
                let meta = metadata_from_values(&values, rng);
                (gold, meta)
            }
        };

        Dataset { item_weights, gold_index, metadata }
    }
}

/// Generate metadata from values for Priority strategy.
fn metadata_from_values(values: &[f64], rng: &mut Lcg) -> Vec<ItemMetadata> {
    values
        .iter()
        .map(|&v| ItemMetadata {
            // Higher value → more activity
            activity: Some(v * 10.0 + rng.next_f64() * 2.0),
            // Higher value → more recent (fewer days since update)
            days_since_update: Some((1.0 - v) * 180.0 + rng.next_f64() * 10.0),
        })
        .collect()
}

// ============================================================================
// Single trial: run strategy on dataset, check if gold is included
// ============================================================================

fn run_trial(
    dataset: &Dataset,
    strategy_kind: TrimStrategyKind,
    budget_tokens: usize,
) -> (bool, usize, usize) {
    let budget = budget_tokens;

    // Build TrimNode tree
    let mut tree = TrimNode::new(0, NodeKind::Root, 0);
    for (i, &weight) in dataset.item_weights.iter().enumerate() {
        let node = TrimNode::new(i + 1, NodeKind::Item { index: i }, weight);
        tree.children.push(node);
    }

    // Assign values based on strategy
    match strategy_kind {
        TrimStrategyKind::Priority => {
            assign_priority_values(&mut tree, &dataset.metadata);
        }
        _ => {
            let strategy = create_strategy(strategy_kind);
            strategy.assign_values(&mut tree);
        }
    }

    // Run trim
    trim::trim(&mut tree, budget);

    // Check results
    let gold_included = tree.children[dataset.gold_index].included;
    let tokens_used = tree.total_weight();
    let items_included = tree.included_items_count();

    (gold_included, tokens_used, items_included)
}

// ============================================================================
// Experiment runner
// ============================================================================

#[derive(Debug)]
struct ExperimentResult {
    dataset_type: &'static str,
    n_items: usize,
    budget_tokens: usize,
    strategy: &'static str,
    p1: f64,
    mean_tokens_used: f64,
    mean_items_included: f64,
    trials: usize,
}

fn run_experiment(
    dtype: DatasetType,
    n_items: usize,
    budget_tokens: usize,
    strategy_kind: TrimStrategyKind,
    trials: usize,
    base_seed: u64,
) -> ExperimentResult {
    let mut hits = 0usize;
    let mut total_tokens = 0usize;
    let mut total_items = 0usize;

    for t in 0..trials {
        let mut rng = Lcg::new(base_seed.wrapping_add(t as u64));
        let dataset = Dataset::generate(dtype, n_items, &mut rng);
        let (gold_included, tokens_used, items_included) =
            run_trial(&dataset, strategy_kind, budget_tokens);

        if gold_included {
            hits += 1;
        }
        total_tokens += tokens_used;
        total_items += items_included;
    }

    ExperimentResult {
        dataset_type: dtype.name(),
        n_items,
        budget_tokens,
        strategy: strategy_kind.as_str(),
        p1: hits as f64 / trials as f64,
        mean_tokens_used: total_tokens as f64 / trials as f64,
        mean_items_included: total_items as f64 / trials as f64,
        trials,
    }
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    let args: Vec<String> = env::args().collect();
    let output_file = args
        .windows(2)
        .find(|w| w[0] == "--output")
        .map(|w| w[1].as_str());
    let trials: usize = args
        .windows(2)
        .find(|w| w[0] == "--trials")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(1000);
    let seed: u64 = args
        .windows(2)
        .find(|w| w[0] == "--seed")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(0xDEADBEEF);

    // Experiment dimensions
    let dataset_types = DatasetType::all();
    let n_items_range = [20usize, 50, 100, 200];
    // Budgets calibrated from real logs: P90 of get_issues = ~10k tokens
    // Practical agent allocation: 1k–8k tokens for tool results
    let budgets = [1000usize, 2000, 4000, 8000];
    let strategies = [
        TrimStrategyKind::Random,
        TrimStrategyKind::Default,      // FIFO-equivalent (uniform values, order preserved)
        TrimStrategyKind::Reversed,
        TrimStrategyKind::ElementCount, // FIFO with position decay
        TrimStrategyKind::Priority,
    ];

    let total = dataset_types.len() * n_items_range.len() * budgets.len() * strategies.len();
    eprintln!("Running {} experiments × {} trials = {} trials total", total, trials, total * trials);

    let mut results: Vec<ExperimentResult> = Vec::with_capacity(total);

    for &dtype in dataset_types {
        for &n in &n_items_range {
            for &budget in &budgets {
                for &strategy in &strategies {
                    let result = run_experiment(dtype, n, budget, strategy, trials, seed);
                    results.push(result);
                }
            }
        }
    }

    // Write CSV
    let header = "dataset_type,n_items,budget_tokens,strategy,p1,mean_tokens_used,mean_items_included,trials\n";
    let csv: String = results
        .iter()
        .map(|r| {
            format!(
                "{},{},{},{},{:.4},{:.1},{:.2},{}\n",
                r.dataset_type,
                r.n_items,
                r.budget_tokens,
                r.strategy,
                r.p1,
                r.mean_tokens_used,
                r.mean_items_included,
                r.trials,
            )
        })
        .collect::<String>();

    match output_file {
        Some(path) => {
            std::fs::write(path, format!("{}{}", header, csv))
                .expect("failed to write output file");
            eprintln!("Results written to {}", path);
        }
        None => {
            print!("{}{}", header, csv);
            io::stdout().flush().unwrap();
        }
    }

    // Print summary table for key scenario: n=50, budget=4k
    eprintln!("\n=== Summary: n=50 items, budget=4000 tokens ===");
    eprintln!("{:<14} {:<14} {:<14} {:<14} {:<14}", "dataset", "strategy", "p1", "items_incl", "tokens");
    for r in results.iter().filter(|r| r.n_items == 50 && r.budget_tokens == 4000) {
        eprintln!(
            "{:<14} {:<14} {:<14.3} {:<14.1} {:<14.0}",
            r.dataset_type, r.strategy, r.p1, r.mean_items_included, r.mean_tokens_used,
        );
    }
}
