//! LLM comprehension eval for TrimTree strategy ablation.
//!
//! Tests whether real LLMs correctly identify a "gold" item after TrimTree compression.
//! All providers use OpenAI-compatible API — one client, different base_url + model_id.
//!
//! # Usage
//!
//! ```shell
//! cp .env.example .env   # fill in your keys
//! cargo run -p llm-eval --release -- --trials 20 --output llm_results.csv
//! cargo run -p llm-eval --release -- --model kimi --trials 5   # single model
//! ```
//!
//! # Output CSV
//!
//! ```
//! model,n_items,budget_tokens,strategy,algo_p1,llm_accuracy,hallucination_rate,trials,latency_ms
//! ```
//!
//! - algo_p1: fraction of trials where gold item is physically included in TrimTree output
//! - llm_accuracy: fraction of trials where LLM correctly names the gold item
//! - hallucination_rate: fraction of trials where gold NOT included but LLM still names it

use std::env;
use std::io::{self, Write as IoWrite};
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use devboy_format_pipeline::strategy::{
    ItemMetadata, TrimStrategyKind, assign_priority_values, create_strategy,
};
use devboy_format_pipeline::token_counter::estimate_tokens;
use devboy_format_pipeline::tree::{NodeKind, TrimNode};
use devboy_format_pipeline::trim;

// ============================================================================
// Model configs
// ============================================================================

#[derive(Debug, Clone)]
struct ModelConfig {
    /// Short name used in CSV output and --model flag
    pub name: String,
    /// OpenAI-compatible base URL (without /chat/completions suffix)
    pub base_url: String,
    /// API key
    pub api_key: String,
    /// Model ID sent in the request
    pub model_id: String,
    /// Max tokens to generate in response
    pub max_tokens: u32,
    /// Provider hint — controls Ollama-only knobs like `reasoning_effort`.
    pub is_ollama: bool,
}

/// Derive a short display name from Ollama model_id: "gemma4:26b" → "gemma4-26b"
fn ollama_short_name(model_id: &str) -> String {
    model_id.replace(':', "-")
}

/// Load .env file into a HashMap (avoids unsafe set_var).
fn load_dotenv(path: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
            }
        }
    }
    map
}

/// Try candidate paths in order; return first non-empty map.
fn load_dotenv_any(candidates: &[&str]) -> std::collections::HashMap<String, String> {
    for path in candidates {
        let map = load_dotenv(path);
        if !map.is_empty() {
            eprintln!("Loaded env from: {}", path);
            return map;
        }
    }
    std::collections::HashMap::new()
}

/// Get value: first from real env, then from dotenv map.
fn getenv(key: &str, dotenv: &std::collections::HashMap<String, String>) -> String {
    env::var(key).unwrap_or_else(|_| dotenv.get(key).cloned().unwrap_or_default())
}

fn load_model_configs(dotenv: &std::collections::HashMap<String, String>) -> Vec<ModelConfig> {
    let mut configs = Vec::new();

    macro_rules! getv {
        ($key:literal) => {
            getenv($key, dotenv)
        };
        ($key:literal, $default:literal) => {{
            let v = getenv($key, dotenv);
            if v.is_empty() {
                $default.to_string()
            } else {
                v
            }
        }};
    }

    // Kimi
    let kimi_key = getv!("KIMI_API_KEY");
    if !kimi_key.is_empty() {
        configs.push(ModelConfig {
            name: "kimi".into(),
            base_url: getv!("KIMI_BASE_URL", "https://api.moonshot.cn/v1"),
            api_key: kimi_key,
            model_id: getv!("KIMI_MODEL", "moonshot-v1-32k"),
            max_tokens: 64,
            is_ollama: false,
        });
    }

    // Zhipu GLM
    let zhipu_key = getv!("ZHIPU_API_KEY");
    if !zhipu_key.is_empty() {
        configs.push(ModelConfig {
            name: "glm".into(),
            base_url: getv!("ZHIPU_BASE_URL", "https://open.bigmodel.cn/api/paas/v4"),
            api_key: zhipu_key,
            model_id: getv!("ZHIPU_MODEL", "glm-4-flash"),
            max_tokens: 64,
            is_ollama: false,
        });
    }

    // Local Ollama (shared base URL)
    let ollama_url = getv!("OLLAMA_BASE_URL", "http://localhost:11434/v1");

    // OLLAMA_MODELS takes precedence if set: "gemma4:26b,gpt-oss:20b,deepseek-r1:14b"
    let ollama_models_list = getv!("OLLAMA_MODELS");
    if !ollama_models_list.is_empty() {
        for model_id in ollama_models_list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            configs.push(ModelConfig {
                name: ollama_short_name(model_id),
                base_url: ollama_url.clone(),
                api_key: "ollama".into(),
                model_id: model_id.to_string(),
                max_tokens: 4096,
                is_ollama: true,
            });
        }
    } else {
        // Fallback: individual GEMMA_MODEL / GPTOSS_MODEL vars
        let gemma_model = getv!("GEMMA_MODEL");
        if !gemma_model.is_empty() {
            configs.push(ModelConfig {
                name: ollama_short_name(&gemma_model),
                base_url: ollama_url.clone(),
                api_key: "ollama".into(),
                model_id: gemma_model,
                max_tokens: 4096,
                is_ollama: true,
            });
        }

        let gptoss_model = getv!("GPTOSS_MODEL");
        if !gptoss_model.is_empty() {
            configs.push(ModelConfig {
                name: ollama_short_name(&gptoss_model),
                base_url: ollama_url,
                api_key: "ollama".into(),
                model_id: gptoss_model,
                max_tokens: 4096,
                is_ollama: true,
            });
        }
    }

    configs
}

// ============================================================================
// Synthetic issue generation (Markdown table — matches real devboy MCP output)
// ============================================================================

struct Issue {
    id: String,
    title: String,
    status: &'static str,
    priority: &'static str,
    comments: usize,
    days_since_update: f64,
}

/// Gold item has CRITICAL priority, many comments, recently updated.
/// Other items are noise with mixed signals.
fn generate_issues(n: usize, gold_index: usize, rng: &mut Lcg) -> Vec<Issue> {
    let titles = [
        "Refactor authentication middleware",
        "Add pagination to search results",
        "Fix memory leak in background worker",
        "Update dependency versions",
        "Improve error messages in API responses",
        "Add rate limiting to public endpoints",
        "Migrate database schema for v2",
        "Fix timezone handling in reports",
        "Optimize slow query in analytics dashboard",
        "Add integration tests for checkout flow",
        "Remove deprecated feature flags",
        "Fix broken image upload on mobile",
        "Add webhook support for third-party integrations",
        "Improve logging in production environment",
        "Fix race condition in job queue",
        "Add support for SAML SSO",
        "Performance regression in list view",
        "Fix XSS vulnerability in comment field",
        "Add export to CSV feature",
        "Cleanup unused API endpoints",
    ];
    let priorities = ["low", "medium", "medium", "high", "low"];
    let statuses = ["open", "open", "in_progress", "open", "open"];

    (0..n)
        .map(|i| {
            if i == gold_index {
                Issue {
                    id: format!("gitlab#{}", 500 + i),
                    title: "Critical: production outage — payment service returning 500".into(),
                    status: "open",
                    priority: "critical",
                    comments: 47,
                    days_since_update: 0.1,
                }
            } else {
                let title_idx = (i + rng.next_usize(3)) % titles.len();
                Issue {
                    id: format!("gitlab#{}", 500 + i),
                    title: titles[title_idx].into(),
                    status: statuses[i % statuses.len()],
                    priority: priorities[i % priorities.len()],
                    comments: rng.next_usize(10),
                    days_since_update: 5.0 + rng.next_f64() * 60.0,
                }
            }
        })
        .collect()
}

// ============================================================================
// TrimTree compression of issue set
// ============================================================================

struct CompressionResult {
    content: String,
    gold_included: bool,
}

fn compress_issues(
    issues: &[Issue],
    gold_index: usize,
    strategy_kind: TrimStrategyKind,
    budget_tokens: usize,
) -> CompressionResult {
    // Build TrimNode tree — each issue = one item node
    let mut tree = TrimNode::new(0, NodeKind::Root, 0);
    for (i, issue) in issues.iter().enumerate() {
        let row = format!(
            "| {} | {} | {} | {} | {} | {}d ago |",
            issue.id,
            issue.title,
            issue.status,
            issue.priority,
            issue.comments,
            issue.days_since_update as u64,
        );
        let weight = estimate_tokens(&row);
        let node = TrimNode::new(i + 1, NodeKind::Item { index: i }, weight);
        tree.children.push(node);
    }

    // Assign values
    if strategy_kind == TrimStrategyKind::Priority {
        let metadata: Vec<ItemMetadata> = issues
            .iter()
            .map(|issue| ItemMetadata {
                activity: Some(issue.comments as f64),
                days_since_update: Some(issue.days_since_update),
            })
            .collect();
        assign_priority_values(&mut tree, &metadata);
    } else {
        create_strategy(strategy_kind).assign_values(&mut tree);
    }

    trim::trim(&mut tree, budget_tokens);

    // Reconstruct compressed markdown from included nodes
    let mut lines = Vec::new();
    lines.push("| ID | Title | Status | Priority | Comments | Updated |".to_string());
    lines.push("|-----|-------|--------|----------|----------|---------|".to_string());
    for (i, issue) in issues.iter().enumerate() {
        if tree.children[i].included {
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {}d ago |",
                issue.id,
                issue.title,
                issue.status,
                issue.priority,
                issue.comments,
                issue.days_since_update as u64,
            ));
        }
    }

    let content = lines.join("\n");
    let gold_included = tree.children[gold_index].included;

    CompressionResult {
        content,
        gold_included,
    }
}

// ============================================================================
// LLM call (OpenAI-compatible)
// ============================================================================

fn call_llm(
    client: &reqwest::blocking::Client,
    config: &ModelConfig,
    issues_content: &str,
    n_total: usize,
) -> Result<(String, u64)> {
    let system = "You are a triage assistant. Reply with ONLY the issue ID in format gitlab#NNN. \
                  No explanation, no preamble, no reasoning. Just the ID.";
    let user = format!(
        "From the following GitLab issues ({n_total} total, some may be omitted), \
         which one needs IMMEDIATE attention? Reply with ONLY its ID (format: gitlab#NNN).\n\n\
         {issues_content}\n\nAnswer:"
    );

    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let mut body = json!({
        "model": config.model_id,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "max_tokens": config.max_tokens,
        "temperature": 0.0,
    });
    // Ollama OpenAI-compat endpoint accepts reasoning_effort for thinking-capable
    // models (gpt-oss, gemma4, qwq, deepseek-r1). On non-thinking models it's a no-op.
    if config.is_ollama {
        body["reasoning_effort"] = json!("high");
    }

    let start = Instant::now();
    let resp = client
        .post(&url)
        .bearer_auth(&config.api_key)
        .json(&body)
        .send()
        .context("HTTP request failed")?;

    let latency_ms = start.elapsed().as_millis() as u64;
    let status = resp.status();
    let text = resp.text().context("failed to read response body")?;

    if !status.is_success() {
        anyhow::bail!("API error {}: {}", status, &text[..text.len().min(200)]);
    }

    let json: Value = serde_json::from_str(&text).context("failed to parse JSON response")?;
    let msg = &json["choices"][0]["message"];

    // With reasoning_effort=high + max_tokens=4096, the model has room to finish
    // its thinking trace and emit the final answer in `content`. We deliberately
    // ignore `reasoning` — it's the thinking stream, not the answer.
    let content = msg["content"].as_str().unwrap_or("").trim();
    let cleaned = strip_code_fence(content);

    Ok((cleaned.to_lowercase(), latency_ms))
}

/// Strip surrounding ```json ... ``` or ``` ... ``` wrappers (gemma4 likes them).
fn strip_code_fence(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        // Drop optional language tag on the first line, then strip trailing fence.
        let after_first_line = rest.split_once('\n').map(|x| x.1).unwrap_or(rest);
        let body = after_first_line.trim_end_matches("```").trim_end_matches('\n').trim();
        return body.to_string();
    }
    s.to_string()
}

/// Check if LLM response correctly names the gold issue.
fn judge(response: &str, gold_id: &str) -> bool {
    // Normalize: "gitlab#524" or "#524" or "524" should all match
    let gold_num = gold_id
        .trim_start_matches("gitlab#")
        .trim_start_matches('#');
    response.contains(gold_id)
        || response.contains(&format!("#{}", gold_num))
        || response.contains(gold_num)
}

// ============================================================================
// LCG (same as eval.rs — no external rand dep)
// ============================================================================

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn next_usize(&mut self, n: usize) -> usize {
        (self.next_f64() * n as f64) as usize
    }
}

// ============================================================================
// Experiment runner
// ============================================================================

#[derive(Debug)]
struct LlmResult {
    model: String,
    n_items: usize,
    budget_tokens: usize,
    strategy: &'static str,
    algo_p1: f64,
    llm_accuracy: f64,
    hallucination_rate: f64,
    trials: usize,
    mean_latency_ms: u64,
}

#[allow(clippy::too_many_arguments)]
fn run_llm_experiment(
    client: &reqwest::blocking::Client,
    config: &ModelConfig,
    n_items: usize,
    budget_tokens: usize,
    strategy_kind: TrimStrategyKind,
    trials: usize,
    seed: u64,
    verbose: bool,
) -> Result<LlmResult> {
    let mut algo_hits = 0usize;
    let mut llm_correct = 0usize;
    let mut hallucinations = 0usize;
    let mut gold_excluded_trials = 0usize;
    let mut total_latency = 0u64;

    for t in 0..trials {
        let mut rng = Lcg::new(seed.wrapping_add(t as u64));
        let gold_index = rng.next_usize(n_items);
        let issues = generate_issues(n_items, gold_index, &mut rng);
        let gold_id = issues[gold_index].id.clone();

        let result = compress_issues(&issues, gold_index, strategy_kind, budget_tokens);

        if result.gold_included {
            algo_hits += 1;
        }

        let (response, latency) = call_llm(client, config, &result.content, n_items)?;
        total_latency += latency;

        let correct = judge(&response, &gold_id);
        if verbose {
            eprintln!(
                "  [t={t}] gold={gold_id} included={} → response: {:?} → {}",
                result.gold_included,
                &response[..response.len().min(120)],
                if correct { "✓" } else { "✗" }
            );
        }
        if correct {
            llm_correct += 1;
        } else if !result.gold_included && judge(&response, &gold_id) {
            // Hallucination: LLM named gold even though it wasn't in context
            hallucinations += 1;
        }
        if !result.gold_included {
            gold_excluded_trials += 1;
        }

        eprint!(".");
    }
    eprintln!(" {}/{} trials done", trials, trials);

    Ok(LlmResult {
        model: config.name.clone(),
        n_items,
        budget_tokens,
        strategy: strategy_kind.as_str(),
        algo_p1: algo_hits as f64 / trials as f64,
        llm_accuracy: llm_correct as f64 / trials as f64,
        hallucination_rate: if gold_excluded_trials > 0 {
            hallucinations as f64 / gold_excluded_trials as f64
        } else {
            0.0
        },
        trials,
        mean_latency_ms: if trials > 0 {
            total_latency / trials as u64
        } else {
            0
        },
    })
}

// ============================================================================
// Main
// ============================================================================

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let model_filter = args
        .windows(2)
        .find(|w| w[0] == "--model")
        .map(|w| w[1].as_str());
    let trials: usize = args
        .windows(2)
        .find(|w| w[0] == "--trials")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(10);
    let output_file = args
        .windows(2)
        .find(|w| w[0] == "--output")
        .map(|w| w[1].as_str());
    let seed: u64 = args
        .windows(2)
        .find(|w| w[0] == "--seed")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(0xC0FFEE);
    let env_file = args
        .windows(2)
        .find(|w| w[0] == "--env-file")
        .map(|w| w[1].clone());
    let verbose = args.contains(&"--verbose".to_string());

    // Load .env — check explicit arg first, then common locations
    let dotenv = if let Some(ref path) = env_file {
        load_dotenv(path)
    } else {
        load_dotenv_any(&[".env", "crates/llm-eval/.env"])
    };

    let all_configs = load_model_configs(&dotenv);
    if all_configs.is_empty() {
        eprintln!("No models configured. Copy .env.example to .env and fill in API keys.");
        eprintln!("At minimum set KIMI_API_KEY or ZHIPU_API_KEY or GEMMA_MODEL or GPTOSS_MODEL.");
        std::process::exit(1);
    }

    let configs: Vec<&ModelConfig> = all_configs
        .iter()
        .filter(|c| model_filter.is_none_or(|f| c.name == f || c.model_id == f))
        .collect();

    if configs.is_empty() {
        eprintln!("No matching model for --model {:?}", model_filter);
        std::process::exit(1);
    }

    eprintln!(
        "Models: {}",
        configs
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Experiment dimensions (smaller than algorithmic eval — LLM calls are slow).
    //
    // Synthetic table rows are ~26 tokens each (≈90 chars / 3.5).
    // Budgets are chosen so 25%, 50%, ~85% of items fit:
    //   n=20 total ≈ 560 tok → budgets 150/300/500
    //   n=50 total ≈ 1340 tok → budgets 250/600/1100
    // This ensures algo_p1 is meaningfully below 1.0 and strategies differ.
    let n_items_range = [20usize, 50];
    let budgets_map: &[(usize, &[usize])] = &[(20, &[150, 300, 500]), (50, &[250, 600, 1100])];
    let strategies = [
        TrimStrategyKind::ElementCount, // FIFO baseline
        TrimStrategyKind::Priority,     // our strategy
    ];

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let total_configs: usize = configs.len()
        * budgets_map.iter().map(|(_, bs)| bs.len()).sum::<usize>()
        * strategies.len();
    eprintln!(
        "Running {} experiment configs × {} trials = {} LLM calls",
        total_configs,
        trials,
        total_configs * trials
    );

    let mut results: Vec<LlmResult> = Vec::new();

    for config in &configs {
        for &n in &n_items_range {
            let budgets = budgets_map
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| *v)
                .unwrap_or(&[]);
            for &budget in budgets {
                for &strategy in &strategies {
                    eprint!(
                        "[{} n={} budget={} strategy={}] ",
                        config.name,
                        n,
                        budget,
                        strategy.as_str()
                    );
                    match run_llm_experiment(
                        &client, config, n, budget, strategy, trials, seed, verbose,
                    ) {
                        Ok(result) => results.push(result),
                        Err(e) => eprintln!("\nERROR: {}", e),
                    }
                }
            }
        }
    }

    let header = "model,n_items,budget_tokens,strategy,algo_p1,llm_accuracy,hallucination_rate,trials,mean_latency_ms\n";
    let csv: String = results
        .iter()
        .map(|r| {
            format!(
                "{},{},{},{},{:.4},{:.4},{:.4},{},{}\n",
                r.model,
                r.n_items,
                r.budget_tokens,
                r.strategy,
                r.algo_p1,
                r.llm_accuracy,
                r.hallucination_rate,
                r.trials,
                r.mean_latency_ms,
            )
        })
        .collect();

    match output_file {
        Some(path) => {
            std::fs::write(path, format!("{}{}", header, csv))?;
            eprintln!("Results written to {}", path);
        }
        None => {
            print!("{}{}", header, csv);
            io::stdout().flush()?;
        }
    }

    // Print summary (middle budget tier for n=50 → 600 tokens)
    eprintln!("\n=== Summary: n=50, budget=600 ===");
    eprintln!(
        "{:<14} {:<14} {:<10} {:<10} {:<10}",
        "model", "strategy", "algo_p1", "llm_acc", "latency_ms"
    );
    for r in results
        .iter()
        .filter(|r| r.n_items == 50 && r.budget_tokens == 600)
    {
        eprintln!(
            "{:<14} {:<14} {:<10.3} {:<10.3} {:<10}",
            r.model, r.strategy, r.algo_p1, r.llm_accuracy, r.mean_latency_ms
        );
    }

    Ok(())
}
