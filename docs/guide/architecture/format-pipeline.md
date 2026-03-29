# Format Pipeline

The format pipeline transforms tool responses into token-efficient output for LLMs using **TOON** (Token-Oriented Object Notation) with intelligent budget trimming and cursor-based pagination.

## Overview

```
ToolOutput (typed data)
    │
    ▼
┌─────────────────────────────────┐
│       Format Pipeline           │
│                                 │
│  1. Build TrimTree              │
│  2. Apply Strategy (values)     │
│  3. Budget Pipeline             │
│     ├─ TOON encode              │
│     ├─ Check budget             │
│     ├─ Trim tree                │
│     └─ Re-encode + verify       │
│  4. Pagination cursor           │
└─────────────────────────────────┘
    │
    ▼
TransformOutput {
    content: String,        // TOON or JSON
    raw_chars: usize,       // input size (JSON)
    output_chars: usize,    // output size (TOON/JSON)
    page_cursor: Option,    // for next page
    agent_hint: Option,     // pagination hint
}
    │
    ▼
FormatResult {
    content: String,            // final text
    metadata: FormatMetadata {
        raw_chars,              // input JSON size
        output_chars,           // output size
        estimated_tokens,       // output_chars * 10 / 35
        compression_ratio,      // output / raw (< 1.0 = savings)
        format,                 // "toon" | "json" | "text"
        truncated,              // budget trimming applied?
    }
}
```

## Output Formats

| Format | Use Case | Token Savings |
|--------|----------|--------------|
| **TOON** (default) | LLM consumption | 3-17% (Full), 44% (Standard), 92% (Minimal) |
| **JSON** | Programmatic processing | baseline |

## TOON Format

[TOON](https://toonformat.dev) (Token-Oriented Object Notation) is a compact, human-readable format designed to minimize token usage when passing structured data to LLMs.

We use the [`toon-format`](https://crates.io/crates/toon-format) Rust crate (v0.4, spec v3.0) — a community-driven, MIT-licensed implementation.

- **Website**: [toonformat.dev](https://toonformat.dev)
- **GitHub**: [toon-format/toon-rust](https://github.com/toon-format/toon-rust)
- **Crate**: [crates.io/crates/toon-format](https://crates.io/crates/toon-format)
- **Spec**: TOON v3.0

Key features used:
- **Key folding**: `data.metadata.items` instead of nested blocks
- **Tabular arrays**: shared headers for arrays of objects
- **Minimal indentation**: 1-space indent

### Example

JSON (16 tokens):
```json
{"users": [{"id": 1, "name": "Alice"}, {"id": 2, "name": "Bob"}]}
```

TOON (13 tokens):
```
users[2]{id,name}: 1,Alice 2,Bob
```

### Trim Levels

The encoder supports three detail levels for progressive degradation:

| Level | Fields | ~Tokens/Issue |
|-------|--------|--------------|
| **Full** | All fields including timestamps, URLs, avatar | ~750 |
| **Standard** | Core fields, no timestamps/avatar | ~400 |
| **Minimal** | Only key, title, state | ~150 |

## Real-World Benchmarks

Benchmarks on popular open-source GitHub projects (budget: 8,000 tokens).

Run your own: `devboy benchmark --owner <owner> --repo <repo>`

### TOON Full vs JSON (no trimming)

| Project | Data | JSON tokens | TOON tokens | Savings | Pages (JSON → TOON) |
|---------|------|-------------|-------------|---------|---------------------|
| **kubernetes/kubernetes** | 30 PRs | 49,443 | 44,870 | **9%** | 7 → 6 |
| **microsoft/vscode** | 28 issues | 24,760 | 23,255 | **6%** | 4 → 3 |
| **microsoft/vscode** | 30 PRs | 18,707 | 16,684 | **11%** | 3 → 3 |
| **rust-lang/rust** | 30 PRs | 15,007 | 13,023 | **13%** | 2 → 2 |
| **rust-lang/rust** | 30 diffs | 7,589 | 6,310 | **17%** | 1 → 1 |
| **golang/go** | 30 issues | 12,217 | 11,022 | **10%** | 2 → 2 |
| **golang/go** | 30 PRs | 12,929 | 11,822 | **9%** | 2 → 2 |
| **facebook/react** | 10 PRs | 8,687 | 8,224 | **5%** | 2 → 2 |
| **meteora-pro/devboy-tools** | 30 PRs | 12,315 | 11,127 | **10%** | 2 → 2 |

### CPU Overhead

TOON encoding costs additional CPU compared to JSON serialization, but the overhead is negligible relative to network latency (~100-500ms for API calls) and LLM inference cost:

| Project | Data | JSON encode | TOON encode | Overhead |
|---------|------|-------------|-------------|----------|
| **kubernetes/kubernetes** | 7 issues | 0.9 ms | 1.2 ms | +30% (+0.3 ms) |
| **kubernetes/kubernetes** | 30 PRs | 3.7 ms | 7.2 ms | +91% (+3.4 ms) |
| **kubernetes/kubernetes** | 16 diffs | 1.4 ms | 1.7 ms | +15% (+0.3 ms) |

The absolute overhead is **< 4ms** even for 30+ items — orders of magnitude less than the token cost savings at LLM inference time.

### TOON with Trim Levels (budget trimming active)

When budget trimming is applied, the pipeline progressively reduces detail level:

| Trim Level | Description | Typical savings vs JSON | Example (25 issues) |
|------------|-------------|------------------------|---------------------|
| **Full** | All fields | 3-17% | 3,979 tokens |
| **Standard** | No timestamps/avatar | ~44% | 2,801 tokens |
| **Minimal** | key + title + state | ~92% | 401 tokens |

The budget pipeline automatically selects the optimal combination:
first tries to fit all items at Full level, then progressively drops to Standard and Minimal for items that don't fit, prioritizing high-value items (determined by the trimming strategy).

### Memory Usage

All allocations are heap-based and freed after processing. No persistent memory overhead.

| Component | Typical (30 items) | Worst case (1000 items) |
|-----------|-------------------|------------------------|
| **TOON encoding** | ~100 KB (intermediate serde_json::Value) | ~2 MB |
| **TrimTree** | ~5 KB (60 nodes × 88 bytes) | ~240 KB |
| **Knapsack DP** (< 100 items) | ~10 KB (after GCD weight scaling) | Falls back to greedy if > 50K |
| **Greedy** (100-999 items) | — | ~25 KB |
| **Output string** | ~30-170 KB (same as result) | ~1 MB |
| **Total peak** | **~150 KB** | **~3 MB** |

The pipeline processes and releases memory synchronously within a single tool call — no background allocations or caches.

### Key Takeaway

- **TOON Full** alone saves 3-17% tokens vs JSON (more with repetitive data structures)
- **Trim Levels** provide the real power: Standard saves ~44%, Minimal saves ~92%
- **Combined with smart trimming**: the pipeline maximizes information within any token budget by keeping the most important items at higher detail and less important items at lower detail or excluded entirely

## Budget Trimming

The trimming problem is modeled as a **Tree Knapsack Problem** (Cho & Shaw, 1997):

> **maximize** Σ p(v) for v ∈ S
> **subject to:** Σ w(v) ≤ B; S is a connected subtree containing root(T)

### Iterative Pipeline

```
1. TOON encode full data → check tokens
2. If ≤ budget → return as-is
3. Calculate B_trim = budget / r × (1 - margin)
4. Loop (max 3 iterations):
   a. Trim tree to B_trim
   b. Re-encode → check tokens
   c. If fits → done
   d. Adjust B_trim based on actual compression ratio
5. Fallback: hard truncate + overflow
```

### Algorithm Selection

| Tree Size (nodes) | Algorithm | Complexity | Optimality |
|---|---|---|---|
| < 100 | Tree Knapsack DP | O(n × B) | Exact optimum |
| 100-999 | Greedy fractional | O(n log n) | ≥ 63% optimum |
| 1,000-9,999 | Hierarchical WFQ | O(n log n) | Proportionally fair |
| ≥ 10,000 | Head+Tail linear | O(n) | Heuristic |

## Trimming Strategies

Each strategy assigns information value to tree nodes based on data type semantics.

### 1. Element Count (`element_count`)

For flat lists (issues, MRs). Value decreases by position: first = 1.0, last = 0.3.

**Tools**: `get_issues`, `get_merge_requests`

### 2. Cascading (`cascading`)

For comments with chronological decay: `p(i) = β^(n-1-i)`, β = 0.95.
Newest comments are most valuable. Oldest of 50 gets ~8% value of newest.

**Tools**: `get_issue_comments`

### 3. Size-Proportional (`size_proportional`)

For diffs, weighted by file type importance:

| File Type | Weight |
|-----------|--------|
| `.lock`, `.sum`, `package-lock.json` | 0.05 |
| `.min.js`, `.min.css` | 0.10 |
| Migrations, schema files | 0.60 |
| Test files | 0.70 |
| Source code | 1.00 |

**Tools**: `get_merge_request_diffs`

### 4. Thread-Level (`thread_level`)

For discussions: resolved = 0.3, unresolved = 1.0.
First and last comment in each thread are always preserved.

**Tools**: `get_merge_request_discussions`

### 5. Head+Tail (`head_tail`)

For logs: 30% head (config/environment), 70% tail (errors/results).
Error patterns (`ERROR|FATAL|Exception|panic`) get boosted value.

**Tools**: `get_job_logs`

### 6. Default (`default`)

Uniform value 1.0 for all nodes. No semantic trimming.

**Tools**: `get_pipeline`, `get_users`, `get_statuses`

## Strategy Resolution

The `StrategyResolver` maps tool names to strategies:

1. **Exact match** in TOML `[format_pipeline.strategies]` overrides
2. **Hardcoded defaults** by tool name
3. **Strip proxy prefix** (`cloud__get_issues` → `get_issues`) and retry 1-2
4. **Fallback** to `default` strategy

## Cursor-Based Pagination

Since the MCP spec does not support pagination in tool responses, we implement it at the middleware level through a `_page_cursor` parameter.

### How It Works

1. First request returns trimmed data + cursor in the response hint
2. Agent includes `_page_cursor` in the next request
3. Pipeline decodes cursor, serves the next page of overflow data
4. Repeat until all data is consumed

### Cursor Format

Base64-encoded JSON:
```json
{
  "v": 1,
  "data_type": "issues",
  "offset": 20,
  "total": 50,
  "strategy": "element_count",
  "budget": 8000
}
```

## Token Estimation

Uses char-based approximation (~3.5 chars/token) instead of tiktoken-rs to avoid ~2MB binary size increase. The 20% margin in the budget pipeline compensates for estimation inaccuracy.

## Crate Structure

```
crates/plugins/format-pipeline/src/
├── lib.rs              # Pipeline, PipelineConfig, OutputFormat, TransformOutput
├── toon.rs             # TOON encoding wrappers + TrimLevel
├── token_counter.rs    # Token estimation
├── tree.rs             # TrimNode structure + builders
├── trim/
│   ├── mod.rs          # Algorithm dispatch
│   ├── knapsack.rs     # Tree Knapsack DP (< 100 nodes)
│   ├── greedy.rs       # Greedy fractional (100-999)
│   ├── wfq.rs          # Hierarchical WFQ (1000-9999)
│   └── head_tail.rs    # Head+Tail linear (≥ 10000)
├── strategy.rs         # 6 strategies + StrategyResolver
├── budget.rs           # Iterative budget pipeline
├── pagination.rs       # Cursor-based pagination
└── truncation.rs       # String/diff truncation utilities
```

## Metadata & Compression Stats

Every `format_output()` call returns `FormatResult` with metadata:

```rust
use devboy_executor::{format_output, FormatResult, FormatMetadata};

let result: FormatResult = format_output(output, Some("toon"), Some("get_issues"), None)?;

println!("Content: {} chars", result.content.len());
println!("Raw JSON: {} chars", result.metadata.raw_chars);
println!("Output: {} chars", result.metadata.output_chars);
println!("Tokens: ~{}", result.metadata.estimated_tokens);
println!("Compression: {:.0}%", (1.0 - result.metadata.compression_ratio) * 100.0);
println!("Truncated: {}", result.metadata.truncated);
```

### NAPI Bridge Integration

When using `format_output()` from a NAPI bridge, serialize `FormatResult` as JSON to expose metadata:

```rust
let result = devboy_executor::format_output(output, format, tool_name, None)?;
let json = serde_json::json!({
    "content": result.content,
    "metadata": result.metadata,
});
// Returns: { content: "...", metadata: { raw_chars, output_chars, estimated_tokens, ... } }
```

> Note: The NAPI `callToolWithMetadata()` function is implemented in the consuming project's NAPI bridge layer, not in this repository.

### Token Estimation

Tokens are estimated as `chars * 10 / 35` (~chars / 3.5), which approximates Claude's tokenizer for mixed English/code content.
