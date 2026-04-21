# Paper 2: Format-Adaptive Tree Encoding via Multi-Choice Knapsack

**Status:** draft  
**Target venue:** ACL 2026 / NAACL 2026  
**Authors:** Andrei Mazniak

---

## Problem

Paper 1 (TrimTree) decides *which* items to include. Paper 2 asks: once we decide to include
a subtree, *how should it be encoded*?

The same data has wildly different token costs depending on format:

| Data shape | JSON | Markdown table | CSV | key:value |
|------------|------|---------------|-----|-----------|
| 20 issues × 5 fields | ~1800 tokens | ~400 tokens | ~280 tokens | — |
| Flat config object | ~200 tokens | — | — | ~80 tokens |
| Code diff | ~500 tokens | — | code-fence | ~500 tokens |

Current approach (TOON) uses a custom format that LLMs don't natively know. The new approach
uses standard formats (Markdown, CSV, key:value) embedded in a Markdown wrapper — formats the
LLM understands without training.

## Core Idea

Extend TrimTree's binary knapsack to a **Multi-Choice Knapsack Problem (MCKP)**:
for each subtree, choose *one format from N options* or skip entirely.

```
For each subtree node i, options j ∈ {skip, kv, csv, table, json, prose}:
  cost(i, j) = tokens when encoded in format j
  value(i, j) = information value (same for all non-skip options)

MCKP: max Σᵢ value(i, jᵢ)  s.t.  Σᵢ cost(i, jᵢ) ≤ budget,  jᵢ ∈ options(i)
```

## Structural Parser

API responses (from DevBoy MCP) are already Markdown tables. We parse them into a typed tree:

```
pulldown-cmark (Markdown) → typed tree nodes:
  SectionNode      (## heading)
  TableNode        (| col | col |) → array of RowNodes
  ListNode         (- item)        → array of ItemNodes
  BlockquoteNode   (> text)        → hint/metadata node (zero cost, always included)
  CodeFenceNode    (``` lang)      → code/diff node

serde_json (JSON) → JsonObjectNode / JsonArrayNode / JsonScalarNode
```

Both parsers produce the same `TreeNode` trait — one MCKP solver handles both.

## Format Selection Rules

Data shape → eligible formats:

| Node type | Eligible formats (ascending token cost) |
|-----------|----------------------------------------|
| Array of objects (issues, MRs) | CSV → Markdown table → JSON array |
| Flat object (config, metadata) | key:value → JSON object |
| Text field (description, body) | truncated string → full string |
| Code / diff | code-fence (fixed, always preserve structure) |
| Numeric / enum | inline → JSON |
| Hint / metadata | blockquote (zero marginal cost) |

## LLM Hints

Overflow items emit a Markdown hint that guides follow-up calls:

```markdown
## Issues (5 of 23, priority-sorted)
> Low-context items below — call `get_issue(id)` for full details.

| #524 | Fix login bug | open | @alex |
| #531 | Upgrade deps | done | @mika |

> [+18 more omitted. Call `get_issues(page=2)` to continue]
```

Hints are `BlockquoteNode` — zero token cost in the budget, always included.
This is the connection to Paper 3: hints reduce enrichment follow-up calls
by proactively telling the agent what's available.

## Structural Markdown Parser Implementation

```rust
// crates/devboy-mcp/src/pipeline/md_tree.rs
pub trait TreeNode {
    fn token_cost(&self, format: Format) -> usize;
    fn eligible_formats(&self) -> &[Format];
    fn encode(&self, format: Format) -> String;
    fn value(&self) -> f64;
}

pub enum Format { Skip, KeyValue, Csv, MarkdownTable, Json, Prose, CodeFence }

pub struct TableNode { pub headers: Vec<String>, pub rows: Vec<Vec<String>> }
pub struct SectionNode { pub title: String, pub children: Vec<Box<dyn TreeNode>> }
// ...
```

## Experiments

1. **Token savings by data shape** — measure tokens(format_j) / tokens(json_baseline)
   per node type across ToolBench RapidAPI dataset (16k real responses).
   Expected: array-of-objects → 70–85% savings with CSV vs JSON.

2. **LLM task accuracy** — does format choice affect LLM accuracy on SWE-bench?
   Run with: JSON baseline / TOON / MCKP-selected format.
   Expected: MCKP ≈ JSON accuracy with 60–75% fewer tokens.

3. **Hint effectiveness** — do hints reduce enrichment follow-up calls?
   Compare E[enrichment_calls] with vs without embedded hints on τ-bench.
   Connects to Paper 3 empirical baseline.

4. **MCKP vs binary knapsack** — does per-subtree format selection improve
   p₁ at the same budget vs Paper 1's binary approach?

## Baselines

- Raw JSON (no optimization)
- TOON format (custom, 3 levels: Full/Standard/Minimal)
- LLMLingua-2 (token-level compression, agnostic to structure)
- ACON (environment observation compression)

## Key Claims

1. MCKP format selection achieves 60–75% token reduction vs JSON with < 3% accuracy loss
2. CSV is optimal for array-of-objects; key:value for flat objects (token savings > 75%)
3. Embedded LLM hints reduce enrichment follow-up calls by ≥ 25% vs no hints

## Implementation Status

- [ ] ТЗ-2: `ResponseEncoder` trait + `ToonEncoder` + `MarkdownTableEncoder` + `CsvEncoder`
- [ ] Structural Markdown parser (pulldown-cmark → TreeNode)
- [ ] MCKP solver (extend binary knapsack with format dimension)
- [ ] LLM hint emission for overflow nodes
- [ ] Token cost model per format (empirical calibration)

## Related Work

- TOON (devboy internal, Paper 1 baseline) — custom 3-level format
- LLMLingua (token dropping) — format-agnostic, loses structure
- RECOMP (extractive compression) — sentence-level, not structure-aware
- Markdown as LLM format: numerous prompting papers confirm LLMs prefer Markdown
- Multi-choice Knapsack Problem: Pisinger 1995, Kellerer et al. 2004
