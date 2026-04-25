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

## In-Response Hints — Design Guide

Response-level hints are short, structured markers embedded in tool output
that (a) point to content elided for budget reasons, (b) reference
earlier tool results identical to the current one, or (c) recap metadata
the agent needs for correct follow-up. They are the primary mechanism by
which the 4-layer pipeline reclaims tokens without losing information the
agent can still retrieve.

### Relation to prior art

| Work | Scope | Granularity | Dynamic (cross-turn)? |
|------|-------|-------------|-----------------------|
| MCP `ToolAnnotations` (2025-03) | tool definition metadata (readOnlyHint, destructiveHint, …) | per-tool | no — static |
| Anthropic *Writing Tools for Agents* (2026) | 25k default budget, pagination / range / filtering, `ResponseFormat` enum | per-response | no — per-call |
| ResourceLink *DualResponseToolResult* (arXiv 2510.05968) | preview + resource-link + `QueryMetadata` with `total_count` | per-response | no — per-call |
| OpenAI function calling guidance | tool-definition token budget, <100 tools, strict schemas | per-tool | no — static |
| **This work** | reference / overflow / format / metadata hints **across turns** | per-response, **session-scoped cache** | **yes** |

The MCP Interest Group has an open agenda item on extending annotations to
responses but no shipped specification; this paper's hint taxonomy is
designed to be forward-compatible.

### Design principles

1. **Markdown blockquote carrier (`> [...]`)** — all in-response hints are
   single-line blockquotes. Most LLMs treat the `>` prefix as metadata and
   parse, but ignore, its payload when generating user-facing text.
   Blockquotes are zero-cost in the MCKP budget (see §Format Selection Rules).
2. **Terseness over prose** — target ≤ 15 tokens per hint (measured in
   `cl100k_base`). See §Hint Cost Model below.
3. **Semantic precision** — a hint must be falsifiable. `> [ref: tc_42]`
   claims byte-identity with `tc_42`; `> [near-ref: tc_42, status: pending→success]`
   claims identity except for the enumerated delta.
4. **Composable with MCP annotations** — response-level hints live in
   `content.text`, not in `ToolAnnotations`; the two carry orthogonal
   information and can coexist without protocol changes.
5. **Agent-interpretable without schema** — a new agent seeing
   `> [ref: tc_42]` for the first time can still retrieve `tc_42` from its
   context by text match. No client-side registry is required.

### Hint taxonomy

Four hint families, emitted by different pipeline layers:

| Family | Emitter | Purpose | Example |
|--------|---------|---------|---------|
| **Reference** | L0 dedup | Byte-identical content already in context | `> [ref: tc_42, byte-identical]` |
| **Near-reference** | L0 dedup (Type-2) | Near-duplicate with small enumerated delta | `> [near-ref: tc_42, status: pending→success, duration: +22s]` |
| **Overflow** | L2 MCKP / Paper 1 knapsack | Items elided for budget; how to retrieve | `> [+18 more omitted. call get_issues(page=2)]` |
| **Format** | L1 / L2 encoders | Non-default encoding of structured payload | `> [encoded: csv from markdown-table | rows=18]` |
| **Metadata** | pipeline preamble | Zero-cost context: query echo, total_count, timestamps | `> [query: "status=open" | total=247 | returned=20]` |

A single response may carry multiple hints in separate blockquotes, but
each hint is independent — clients can process or ignore them individually.

### Format specification

All hints share the form

```
> [<family>: <payload>]
```

where `<family>` is one of `ref` / `near-ref` / `+N more` / `encoded` / `query`,
and `<payload>` is family-specific. The leading `> ` (greater-than, space)
and trailing newline are required.

#### Reference (L0 hit)

```
> [ref: <tool_call_id_hash>, byte-identical]
```

- `<tool_call_id_hash>`: 8-character hex prefix of SHA-256 of the referenced
  response content. Identifies the prior call without exposing its body.
- `byte-identical`: signals to the agent that the content hash matched
  exactly — the agent should retrieve tokens from its in-context copy of
  that earlier tool response.

Measured cost: **8 tokens** for a 16-char hash, **11 tokens** for an
8-char hash with extra metadata (`cl100k_base`). See §Hint Cost Model.

#### Near-reference (L0 Type-2, future work)

```
> [near-ref: <id>, <change-list>]
```

Where `<change-list>` is `key: old_value→new_value` pairs separated by `, `.
Intended for high-frequency, near-idempotent endpoints like pipeline polls
where only `status` and `duration` change between calls.

Measured cost: **13-18 tokens**. Applicable when the delta is ≤ 30 bytes
and the full response would have been ≥ 500 bytes.

#### Overflow (L2 knapsack / Paper 1)

```
> [+<N> more. call <followup_signature>]
```

- `<N>`: count of elided items.
- `<followup_signature>`: exact tool call the agent can issue to retrieve
  them (e.g. `get_issues(page=2, status="open")`).

The paired *header* preceding the blockquote carries total and slice:

```
## Issues (5 of 23, priority-sorted)
```

Measured cost: **14-22 tokens** depending on signature length.

#### Format (L1/L2)

```
> [encoded: <format_id> | <salient-parameters>]
```

Where `<format_id>` ∈ {`csv_from_md`, `deep_mckp`, `kv`, `csv`, `toon`} and
`<salient-parameters>` summarizes what the agent should expect (e.g. `rows=18, cols=6`).
Rarely emitted — most L1/L2 encodings are self-describing. Emit when the
format differs markedly from the endpoint's historical default.

Measured cost: **12-15 tokens**.

#### Metadata (preamble)

```
> [query: "<query>" | total=<N> | returned=<K>]
```

Purely informational. Emitted at the top of responses where the agent's
subsequent reasoning benefits from knowing scope (e.g. "got 20 of 247
results — I may need pagination"). Zero-cost policy: emit only if the
saved tokens from *avoiding an unnecessary follow-up call* outweigh the
hint cost. Empirically, worth emitting for any tool where
`total_count > returned_count`.

Cost: **10-18 tokens** depending on query length (see Paper 3 for
follow-up call reduction measurements).

### Hint cost model

Let `t(h)` be the tokenized length of hint `h` under `cl100k_base`.
Measurements on our corpus (mean over 100 samples per family):

| Hint family | Mean tokens | Minimum | Maximum |
|-------------|-------------|---------|---------|
| Reference (`> [ref: X, byte-identical]`) | **11** | 8 | 14 |
| Near-reference (`> [near-ref: X, s: a→b]`) | 14 | 11 | 22 |
| Overflow (`> [+N more. call F]`) | 16 | 12 | 24 |
| Format (`> [encoded: F \| rows=N]`) | 13 | 10 | 18 |
| Metadata (`> [query: Q \| total=N]`) | 14 | 10 | 20 |

The **break-even** point — response size above which emitting a hint saves
tokens — is approximately 4× the hint's own cost (because the alternative
is a full response with the same information).

| Hint family | Break-even response size |
|-------------|--------------------------|
| Reference | ≥ 44 tokens (~175 chars) |
| Near-reference | ≥ 56 tokens (~220 chars) |
| Overflow | ≥ 64 tokens (~255 chars) |
| Format | ≥ 52 tokens (~210 chars) |

Most tool responses exceed these thresholds by 10-100×, so hint emission
is nearly always profitable when applicable.

### Semantic guarantees

For each hint we state an agent-testable invariant:

- **Reference**: if the agent retrieves `tc_X` from its context, the bytes
  it obtains are byte-identical to what would have been emitted in lieu
  of the hint.
- **Near-reference**: the claimed delta is the *exact* diff; all other
  bytes match.
- **Overflow**: reissuing the printed `<followup_signature>` returns the
  elided items, modulo the data source's own consistency guarantees (no
  snapshot isolation promised).
- **Format**: decoding `<format_id>` with standard parsers yields the
  logical content the agent would have received from the raw endpoint.
- **Metadata**: the numeric fields are accurate at the time of emission;
  no freshness guarantee across turns.

The pipeline MUST NOT emit a hint whose invariant it cannot satisfy. On
content mutation or compaction boundary, the relevant cache entries are
invalidated (see §L0 Dedup) before any hint can reference them.

### Compatibility with MCP `ToolAnnotations`

The two are strictly orthogonal:

| Aspect | MCP `ToolAnnotations` | Paper 2 in-response hints |
|--------|-----------------------|---------------------------|
| Location | `tool.annotations` field in tool registration | `content.text` body of each result |
| Granularity | tool-level, static | response-level, dynamic |
| Purpose | safety/UX (auto-approval, confirmation) | token economy, pagination, history reference |
| Schema | strongly typed (booleans + title) | free-form blockquote markup |
| Trust | advisory; client decides | advisory; client decides |
| Backward compat | shipping since 2025-03-26 | forward-compat with open SEP for response annotations |

A server MAY emit Paper 2 hints regardless of whether it publishes
`ToolAnnotations`; a client MAY parse hints regardless of whether it
enforces annotations. Neither mechanism strictly depends on the other.

### Alignment with Anthropic & OpenAI guidance

- Anthropic (*Writing Tools for Agents*, 2026): hint-based dedup operationalizes
  their "steer agents towards more token-efficient tool-use behaviors"
  guidance at the infrastructure layer — the agent needs no prompting to
  benefit.
- Anthropic `ResponseFormat` enum (detailed/concise): our adaptive configuration
  (§Adaptive Configuration) provides a data-driven generalization — the
  right verbosity is learned per-endpoint from telemetry rather than chosen
  per-call by the agent.
- OpenAI *Function Calling* guidance: focuses on tool-definition tokens;
  complementary with our work on tool-response tokens.
- ResourceLink `DualResponseToolResult` (arXiv 2510.05968): overlap on
  overflow handling. Our Overflow hint format maps directly onto their
  `total_count` + `returned_count` semantics; a server can emit both for
  redundancy.

### Anti-patterns

Examples of hints the pipeline must not emit:

| Anti-pattern | Why |
|--------------|-----|
| `> [ref: tc_42, last modified 3 min ago]` | Freshness claim without compaction-boundary guarantee |
| `> [ref: tc_42]` without the earlier call still in context | Dangling reference; breaks retrievability invariant |
| `> [encoded: custom_json_v3]` | Uses a format_id no public parser recognizes |
| `> [skip: too big, sorry]` | Unactionable; no recovery path for the agent |
| Multi-line blockquote fences | Parser fragility across clients; stay single-line |

### Legacy example from Paper 2 v1

The original overflow example remains valid under the new taxonomy:

```markdown
## Issues (5 of 23, priority-sorted)
> [query: "status=open" | total=23 | returned=5]

| #524 | Fix login bug | open | @alex |
| #531 | Upgrade deps | done | @mika |

> [+18 more. call get_issues(page=2)]
```

Three hints in one response:
1. Metadata preamble — query echo + counts.
2. The MCKP-selected `markdown_table` body (not itself a hint).
3. Overflow footer — signal to the agent about continuation.

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

## Real-World Applicability (Claude Code JSONL Corpus)

To ground the MCKP framework in real usage, we analyzed **144,001 tool-call
responses** extracted from anonymized Claude Code session logs. Each response
was classified by structural shape, scanned for embedded inner formats
(diff / log / md / xml / yaml / code), and evaluated against a recursive
deep-MCKP cost model.

**Extraction pipeline**: `docs/research/scripts/extract_paper2_format_events.py`
(anonymizing — emits only shape, size, format counts; never raw content).

### Shape distribution

| Shape | Events | % | Total tokens |
|-------|-------:|---:|-------------:|
| prose (bash / grep / free text) | 76,720 | 53.3% | 35M |
| numbered_list (file reads) | 47,822 | 33.2% | 59M |
| nested_object | 6,880 | 4.8% | 7.5M |
| code_block | 6,334 | 4.4% | 7.9M |
| bullet_list | 4,909 | 3.4% | 2.6M |
| markdown_table | 1,051 | 0.7% | 1.3M |
| flat_object | 183 | 0.1% | 0.1M |
| array_of_objects | 98 | 0.1% | 0.1M |

Paper 2's target pool — shapes where format selection is meaningful —
is 8,212 events (5.7% of corpus). The remaining 94% are text forms
(file reads, bash output, prose) that cannot be re-encoded.

### Multi-format (matryoshka) prevalence

Of the 8,212 structured responses, **83% of `nested_object` events contain
formats at depth ≥ 3** (e.g. `nested_object > url + log + hash` inside
string-valued fields). The canonical case: MCP `get_*_pipeline` responses
with 13–21 URL fields, 2 log fields, and a commit hash inline, all wrapped
in a JSON top level.

### Top inner-format combinations

| Container | Inner formats | Events | Avg savings (deep_mckp) | Ktok saved |
|-----------|---------------|-------:|------------------------:|-----------:|
| nested_object | **log + url + hash** (pipeline sig.) | 1,432 | 18.7% | 263 |
| nested_object | log + url | 1,434 | 3.3% | 42 |
| nested_object | diff (MR diffs wrapped in JSON) | 151 | 5.1% | 34 |
| nested_object | prose + url + log (issue body) | 1,122 | 2.7% | 7 |

### Corpus-level impact

- **Applicable events (≥ 20% savings): 1,797** (1.25% of corpus)
- **Total tokens saved: 1.55M** (1.35% of baseline)
- **Dominant wins**:
  - `csv_from_md` (markdown table re-encoding): 883 events, **1,120 ktok**
    (avg 69% savings). Primary beneficiaries: MCP `get_issues` endpoints
    (avg ~92% savings per call).
  - `deep_mckp` (recursive per-leaf format selection): 867 events, **426 ktok**
    (avg 26% savings). Primary beneficiaries: MCP `get_*_pipeline` endpoints.
- **Savings bimodal**: two clear peaks — 20-30% bucket (pipelines, 930 events,
  408 ktok) and 90%+ bucket (big tables, 411 events, 1,021 ktok).

### Rule set distilled from the corpus

Full YAML at `docs/research/data/paper2_format_rules_v2.yaml`. Top 4 rules
cover 95.8% of all savings:

| Rule | Predicate | Action | Savings (median) | Support | Ktok |
|------|-----------|--------|------------------:|--------:|-----:|
| R1 | md_table + cols≥2 + chars>8k | csv_from_md | **99.6%** | 200 | **850** |
| R8 | nested + log+url+hash | deep_mckp | **20.1%** | 1,432 | **369** |
| R3 | md_table + cols 2–7 (small) | csv_from_md | 35.5% | 659 | 158 |
| R2 | md_table + cols≥8 | csv_from_md | 97.3% | 177 | 108 |

### Implications for the MCKP algorithm

1. **Markdown-table → CSV is the primary optimization.** The canonical
   Paper 2 code path should be a streamlined md→CSV re-encoder with
   automatic emission for any response with `shape == markdown_table AND
   n_cols ≥ 2`. No multi-choice decision needed in this path — CSV always
   wins.

2. **Deep recursion is required for nested objects.** Flat top-level MCKP
   misses 83% of savings on `nested_object` because the benefit lives in
   string-valued leaves (logs, diffs, URLs). The solver must recurse into
   string leaves with a format sniffer before deciding emission.

3. **Pipeline responses deserve a template.** The `log+url+hash` trifecta
   is so consistent (1,432 events, 50% applicable) that a
   `pipeline_encoder` specialization saves more than a generic MCKP.

4. **Bash does not need Paper 2.** 0.4% of 59k Bash responses are
   applicable. Optimization effort here is misplaced — focus on MCP.

## Adaptive Configuration

A single static configuration for the 4-layer pipeline leaves substantial
savings on the table. Agent workloads differ on several axes: the ratio
of file-search to MCP calls, endpoint fingerprint distributions, session
length, and compaction frequency all vary within an order of magnitude
across users even in the same team. This section specifies the metrics
the pipeline should collect and the rules by which those metrics drive
configuration choices.

### Why defaults are insufficient

Three representative profiles from our corpus illustrate the gap:

| Profile | Characteristics | Default-config savings | Tuned-config savings | Δ |
|---------|------------------|-----------------------:|----------------------:|---:|
| **A. File-search heavy** — 65% `Read` calls, duplicate rate 40% | mid-size (100–499 events) | 34% | 41% | +7 pp |
| **B. MCP pipeline monitor** — 30% polling calls, deep-nested JSON | small (20–99 events) | 28% | 44% | +16 pp |
| **C. Marathon refactor** — 2000+ events, 46 compactions | very long, mixed | 22% | 32% | +10 pp |

Defaults were chosen to minimize worst-case regression, not to maximize
per-profile savings. A per-profile tuner lifts average savings from 28% to
**39%** on the same corpus — a relative improvement of +40%.

### Tuning knobs

Twelve knobs span three layers. Each is keyed on telemetry measurable at
per-response granularity:

| Layer | Knob | Type | Default |
|-------|------|------|---------|
| L0 | `dedup.lru_size` | int ≥ 1 | 5 |
| L0 | `dedup.enabled_per_endpoint` | bool map | all-true |
| L0 | `dedup.hint_verbosity` | `terse`/`standard`/`verbose` | `standard` |
| L0 | `dedup.near_ref_enabled` | bool | false |
| L0 | `dedup.min_body_chars` | int | 200 |
| L1 | `templates.active` | list[template_id] | built-in 3 |
| L1 | `templates.endpoint_overrides` | map[endpoint → template_id] | empty |
| L2 | `mckp.recursion_depth` | int | 5 |
| L2 | `mckp.formats_enabled` | set of format_ids | all |
| L2 | `mckp.shape_thresholds` | map[shape → min_match] | built-in |
| Meta | `telemetry.sample_rate` | float 0.0-1.0 | 1.0 |
| Meta | `telemetry.flush_every_n` | int | 25 |

### Metrics-to-knobs decision rules

The tuner is rules-based (not ML) for explainability. Each rule reads
aggregated telemetry and emits a config delta:

```
R1  per-endpoint dedup:
    for each endpoint E:
      if E.dup_rate ≥ 0.30 → dedup.enabled_per_endpoint[E] = true
                             and suggest lru_size = min(10, 1 + round(E.repeat_burst_p90))
      if E.dup_rate ≤ 0.05 → dedup.enabled_per_endpoint[E] = false

R2  per-endpoint template:
    for each endpoint E:
      if E.shape_dominant == "markdown_table" and E.avg_cols ≥ 2:
          templates.endpoint_overrides[E] = "csv_from_md"
      elif {"log","url","hash"} ⊆ E.inner_formats_top3:
          templates.endpoint_overrides[E] = "pipeline_deep_mckp"
      elif "diff" in E.inner_formats_top3:
          templates.endpoint_overrides[E] = "mr_diff_fence"

R3  LRU sizing:
    if session_profile.compaction_rate > 0.05 events⁻¹:
        dedup.lru_size = max(10, ceil(session_profile.avg_events_per_partition * 0.2))
    elif session_profile.avg_events < 20:
        dedup.lru_size = 3
    else:
        dedup.lru_size = 5

R4  MCKP recursion:
    if nested_object_depth_p95 < 3:
        mckp.recursion_depth = 3
    else:
        mckp.recursion_depth = 5

R5  min_body_chars:
    dedup.min_body_chars = clamp(p25(response_chars), 100, 500)
```

Each rule is accompanied by a measured effect size on the corpus
(reported in Implementation Status section once instrumented).

### Architecture

```
┌────────────────────┐       ┌────────────────────┐
│ Pipeline (Rust)    │──────►│ TelemetrySink      │
│ L0/L1/L2/L3        │       │ (per-session JSONL)│
└─────────┬──────────┘       └──────────┬─────────┘
          │                             │
          ▼                             ▼
  formatted response         ~/.config/devboy/telemetry/
                                       │
                            ┌──────────┴──────────┐
                            ▼                     │
                       `devboy tune analyze`      │ (weekly/manual)
                            │                     │
                            ▼                     │
                  pipeline_config.toml ◄──────────┘
                            │
                            ▼
                  Pipeline::with_config()
                  (next session picks up tuned knobs)
```

The tuner is **offline**: no runtime reconfiguration mid-session. This
keeps the hot path free of control-plane logic and makes behavior
reproducible for a given config file.

## Configuration Extensibility

The tuner described above mines pipeline telemetry. That presupposes the
pipeline is already running and emitting events. To bootstrap a brand-new
user *and* to adapt to per-axis variation that the rule set in
§Adaptive Configuration cannot express, schema v2 introduces four
orthogonal **profile axes** plus a horizontal **hint policy**.

### Why four axes (and not more knobs on one config)

The 2026-04-25 evaluation surfaced a class of measurements that no global
knob can encode. For example: inline-JSON cells in a Markdown table cost
+119% prompt tokens on `glm-5.1` (Anthropic-class tokenizer) but ±0% on
local Ollama models (BPE). The encoder choice is not the problem — the
*receiving model's tokenizer* is. Likewise, `schema_explainer` hints add
+0 p.p. accuracy on capable cloud models but unlock the remaining 5
errors on `gpt-oss:20b`. Capturing this without an explosion of
endpoint-specific exceptions requires factoring the configuration along
the dimensions that actually vary.

### The four axes

```
┌─ profiles ──────────────────────────────────────────────────────────┐
│                                                                     │
│  tokenizer   {anthropic_class | openai_o200k | ollama_bpe | …}      │
│      │ chars_per_token, inline_json_cost, toon_overhead             │
│      ▼                                                              │
│  llm         model_id → tokenizer + context_window + style flags    │
│      │ resolves "auto" against SessionContext.model_id              │
│      ▼                                                              │
│  agent       {default | file_search_heavy | marathon_refactor}      │
│      │ priority (latency/balanced/accuracy), recursion_depth,       │
│      │ hint_aggressiveness, near_ref_enabled                        │
│      │ resolves "auto" by classifying session stats                 │
│      ▼                                                              │
│  data        endpoint_pattern → preferred_format + hint_set         │
│              first match wins; placeholder variants auto-registered │
│              for unknown observed endpoints                         │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

Plus, at the same level as `[profiles]`, a **`[hints]`** policy with
per-type rules (`enabled`, `max_per_session`, `applies_to_models`) gates
every emit through one chokepoint. Every axis defaults to `active =
"auto"` so a user with no explicit overrides still gets a coherent view
resolved at session start.

### Resolution chain

```
SessionContext { model_id, stats { event_count, compaction_count, read_share } }
        │
        ▼
profiles.llm.resolve(model_id)        ──► LlmProfile (incl. tokenizer ref)
        │
profiles.tokenizer.get(llm.tokenizer) ──► TokenizerProfile (cost model)
        │
profiles.agent.resolve(stats)         ──► AgentProfile (recursion_depth, …)
        │
hints                                 ──► HintsConfig (allow(type, model))
        │
        ▼
EffectiveConfig — single value the LayeredPipeline reads on the hot path
```

The collapse is a pure function: same config + same context → same
EffectiveConfig. There is no run-time learning inside the pipeline; the
adaptation lives entirely in offline tuning.

### Built-in defaults (excerpt)

| Axis | Variant | Where it wins |
|---|---|---|
| **tokenizer** | `anthropic_class` | `chars_per_token = 3.5`, `inline_json_cost = 2.2`, `toon_overhead = 1.13`. Chosen automatically when the LLM profile points to Sonnet, Opus, or GLM. |
| **tokenizer** | `openai_o200k` | `chars_per_token = 4.0`, `toon_overhead = 0.60` (the only tokenizer where TOON's −40% claim holds). |
| **tokenizer** | `ollama_bpe` | Used by every local Ollama LLM profile — inline-JSON cells are free here, so `mckp_v2` collapses to `mckp_v1` cost without the data loss. |
| **llm** | `glm-5.1` | `tokenizer = anthropic_class`, `context_window = 128k`, `prefer_explicit_keys = true`, `max_inline_nested = 128`. |
| **llm** | `claude-sonnet-4.6` | Same tokenizer family, `context_window = 200k`, tighter `max_inline_nested = 64` (cells are expensive on this tokenizer). |
| **llm** | `gpt-oss:20b` / `gemma4:26b` | `tokenizer = ollama_bpe`, `context_window = 8 192`, `prefer_explicit_keys = false`, `max_inline_nested = 512`. |
| **agent** | `default` | Balanced, `recursion_depth = 5`, `hint_aggressiveness = 0.5`. |
| **agent** | `file_search_heavy` | Activates when ≤200 events and read-share ≥0.5. Drops `recursion_depth` to 3 and `hint_aggressiveness` to 0.3 — latency-prioritised. |
| **agent** | `marathon_refactor` | Activates when ≥500 events and ≥3 compactions. Lifts `recursion_depth` to 7, enables `near_ref` hints. |
| **data** | `gitlab_issues`, `github_pulls`, `k8s_logs`, `mr_diffs` | Pre-shipped endpoint-pattern → preferred-format + hint-set bindings. |
| **hints** | `schema_explainer` | **`enabled = false`** — confirmed 0 p.p. lift in 2026-04-25 evaluation; documenting the negative result keeps future tuners from rediscovering it. |
| **hints** | `inline_format_hint` | `applies_to_models = ["gpt-oss:20b", "gemma4:26b"]` — only fires for the local models that benefit. |

### CLI: `devboy tune from-claude-logs`

When a user has no pipeline telemetry yet (a fresh install, or a new
project), the agent already collected most of the necessary signal in
its session log. The new subcommand `devboy tune from-claude-logs`:

1. Walks `~/.claude/projects/<project>/*.jsonl` (or any `--input-dir`).
2. Counts model ids, tool invocations (read-class vs other), `mcp__*`
   endpoint hits, sessions, and `/compact` events.
3. Applies three rules — **P1** dominant-model pin (≥80% share),
   **P2** agent classifier from session stats, **P3** placeholder
   data-profile registration for every observed `mcp__*` endpoint.
4. Writes (or, with `--dry-run`, prints) the proposed
   `pipeline_config.toml`.

This complements the existing `devboy tune analyze` (which mines
emitted telemetry events) and lets a fresh install converge to a
near-tuned configuration on first run.

### Skill: `devboy-pipeline-tune`

`skills/00-self-bootstrap/devboy-pipeline-tune/SKILL.md` is a procedural
recipe for an agent driving the CLI on a user's behalf:

1. Sanity-check that `~/.claude/projects` exists and has data.
2. Run with `--dry-run` first; surface the proposed pins to the user.
3. Apply on confirmation; verify with `devboy tune show`.
4. Per-tool refinement: ask the user what shape each `mcp__*` endpoint
   returns, fill in `preferred_format`.
5. Watch for accuracy regressions in subsequent sessions.

The skill is opinionated about anti-patterns: don't enable
`schema_explainer`, don't pin a model whose tokenizer profile is wrong,
don't run across mixed-project log directories.

### Schema v1 → v2 migration

`AdaptiveConfig::load` accepts both schemas. When loading a v1 file the
deserializer uses `serde(default)` to populate the new `[profiles]` and
`[hints]` sections from the v2 defaults, then bumps `schema_version` in
memory. Saving back writes a v2 file. The migration is one-way (v2 cannot
be read by v1 binaries) but the on-disk v1 file is left untouched until
the user explicitly saves.

A future schema version is rejected at load time
(`ConfigError::UnsupportedSchemaVersion`) — old binaries won't silently
mis-parse a newer file.

## Telemetry & Observability

### Per-response event (atomic unit)

Emitted once per tool-result. Schema (stable; additions only):

| Field | Type | Description |
|-------|------|-------------|
| `session_hash` | string(16) | Partition key, sha-256 prefix of session UUID |
| `tool_call_id_hash` | string(8) | Identity of this response (for hint refs) |
| `tool_name_anon` | string | Anonymized tool name (MCP slugs hashed) |
| `endpoint_class` | string | Inferred endpoint (Bash → `git_log` / `curl` / …) |
| `response_chars` | int | Raw byte count |
| `shape` | enum(9) | `prose` / `numbered_list` / `nested_object` / … |
| `inner_formats` | set[string] | Embedded formats detected inside string leaves |
| `content_sha_prefix` | bytes(16) | 128-bit content fingerprint (for dup detection) |
| `file_path_hash` | string(8)? | For `Read`/`Edit` tools, invalidation key |
| `is_dedup_hit` | bool | Was an L0 hint emitted? |
| `layer_used` | enum(L0,L1,L2,L3) | Terminal layer in the pipeline decision |
| `template_id` | string? | L1 template name if used |
| `tokens_baseline` | int | chars / 4 or tiktoken measurement |
| `tokens_final` | int | After pipeline encoding |
| `context_partition` | int | Monotonic counter, increments on compaction |
| `is_sidechain` | bool | Subagent vs main session |
| `ts_ms` | int64 | Unix milliseconds |

No raw text, no tool arguments, no user-facing strings. Field additions
are backward-compatible; the sink writes JSONL and downstream analyzers
use field-level projection.

### Per-session summary (rolled up on session close)

| Field | Type | Description |
|-------|------|-------------|
| `session_hash` | string(16) | — |
| `total_events` | int | — |
| `dedup_hit_rate` | f32 | L0 hits / events |
| `l1_hit_rate`, `l2_hit_rate` | f32 | template / generic-mckp fractions |
| `avg_response_chars` | f32 | — |
| `compaction_count` | int | `context_partition` max |
| `endpoint_mix` | map[endpoint → count] | top-20 endpoints |
| `total_baseline_tokens` | int | — |
| `total_final_tokens` | int | — |
| `savings_pct` | f32 | 1 − final/baseline |
| `duration_sec` | f32 | — |
| `ended_at_ms` | int64 | — |
| `sample_rate_applied` | f32 | if session was down-sampled |

### Per-endpoint fingerprint (rolling 30-day window)

Aggregated across sessions for each unique endpoint. Used by the tuner;
also the unit a team may choose to share with the endpoint's upstream
provider.

| Field | Type | Description |
|-------|------|-------------|
| `endpoint_class` | string | — |
| `call_count` | int | Within window |
| `avg_response_chars` | f32 | — |
| `shape_dominant` | string | Most common shape |
| `shape_mix` | map[shape → frac] | Full distribution |
| `dup_rate` | f32 | Responses that matched an earlier one in same partition |
| `format_entropy` | f32 | Shannon entropy of shape mix; low = stable fingerprint |
| `inner_formats_top3` | list[string] | Three most common inner formats |
| `avg_cols`, `avg_rows` | f32 | When shape is `markdown_table` |
| `key_stability_mean` | f32 | When shape is `array_of_objects` |
| `first_seen_ms`, `last_seen_ms` | int64 | Window bounds |
| `source_sessions` | int | k-anonymity indicator |

### Storage & aggregation format

| Tier | Format | Path | Lifecycle |
|------|--------|------|-----------|
| Raw events | JSONL (append-only) | `~/.config/devboy/telemetry/events/<session>.jsonl` | Kept 30 days, then rotated |
| Session summaries | JSONL | `~/.config/devboy/telemetry/sessions.jsonl` | Kept 90 days |
| Endpoint fingerprints | Parquet (rolling window) | `~/.config/devboy/telemetry/endpoints.parquet` | Overwritten on each tune pass |
| Tuned config | TOML (human-readable) | `~/.config/devboy/pipeline_config.toml` | Version-controlled if desired |

### Privacy & scoping tiers

| Tier | Data visibility | Required consent |
|------|-----------------|------------------|
| **User-local** (default) | Raw events, session summaries, endpoint fingerprints stay on the user's machine. Tuner reads them to produce `pipeline_config.toml`. | Implicit: writing to user's home directory |
| **Team-shared** | *Endpoint fingerprints only* (no session-level, no raw events) are uploaded to a team bucket for joint tuning. Fingerprints are already aggregated; sessions are counted but not enumerated. k-anonymity threshold ≥ 5 before an endpoint is shared. | Explicit opt-in via `telemetry.share_endpoints_with_team = true` |
| **Provider-shared** | For a specific MCP server, its own endpoints' fingerprints are uploaded to the server provider (e.g., to inform their server-side optimizations). No cross-provider mixing. | Explicit opt-in via `telemetry.share_with_provider[<server-id>] = true` |

No tier shares raw response content, tool arguments, or user prompts. The
endpoint_class string, designed to be coarse (e.g., `git_log`, not the full
command), is the finest granularity that leaves the user's machine.

## Deployment Patterns

### A. Single developer (default)

The full loop runs on one machine with no network dependencies:

```
1. Developer uses Claude Code; devboy-mcp middleware logs per-response
   events to ~/.config/devboy/telemetry/events/<session>.jsonl.
2. At session close, a rollup writes the session summary.
3. Weekly (or manual), `devboy tune analyze --days 30` aggregates events
   and fingerprints, runs rules R1-R5, writes pipeline_config.toml.
4. Next session's Pipeline loads the updated config.
```

No data leaves the machine. The developer's own workload shapes the
algorithm's behavior.

### B. Team — shared endpoint fingerprints

A team sharing the same MCP deployments (e.g., one GitLab instance) can
pool endpoint fingerprints without exposing individual session data:

```
1. Each member runs User-local as in pattern A.
2. With `telemetry.share_endpoints_with_team = true` enabled, the tuner
   uploads each member's `endpoints.parquet` to a shared bucket
   (S3 / GCS / shared FS) after applying k-anonymity (≥ 5 sessions per
   endpoint before it's included).
3. A team aggregator merges member fingerprints, producing a
   `team_endpoints.parquet` weighted by session count.
4. Each member's tuner reads `team_endpoints.parquet` and their own
   `endpoints.parquet`, blending them (default: 70% team / 30% self)
   before running rules.
5. The resulting `pipeline_config.toml` is checked into the team's repo
   as `.devboy/pipeline_config.toml` and applied by all members.
```

Benefits: high-volume endpoints in the shared deployment get accurate
statistics even for team members who rarely call them. Trade-off:
onboarding a member pulls the team config, which may mask their
personal workload idiosyncrasies for a week until their own data
reweights the blend.

### C. MCP provider — publishing native fingerprints

An MCP server operator who wants their tools to be used most efficiently
can publish per-endpoint fingerprints as part of the server's metadata:

```
1. The server instruments responses (or consumes anonymous client-side
   fingerprints via the Provider-shared opt-in).
2. The server exposes a standard endpoint, e.g.,
   GET /mcp/fingerprints/v1 → list of { endpoint_class, shape_dominant,
   inner_formats_top3, dup_rate, ... }.
3. Clients' tuners fetch these fingerprints alongside their own telemetry
   and use them as strong priors (especially for endpoints the client has
   seen rarely).
```

Benefits: a new user of a server benefits from the aggregate experience
of all users on the first session, not after a week of local telemetry.
This pattern is forward-compatible with the open MCP SEP on
response-level annotations — if standardized, endpoint fingerprints can
become part of the protocol rather than a server-specific extension.

### Failure modes & mitigations

| Risk | Mitigation |
|------|------------|
| Tuner produces a config that regresses savings vs default | Every tune pass writes a before/after simulation on the developer's local corpus; if savings drop, the new config is flagged and the old one is retained. |
| Team config drifts from reality after deployment change | TTL on team fingerprints (default: 14 days); stale endpoints demoted. |
| Small-corpus overfitting | Rules R1/R2 require a minimum `call_count ≥ 20` per endpoint before activating a non-default template. |
| Provider publishes biased fingerprints | Client blends provider data at most 40% weight; local telemetry is always the majority vote. |
| Sample-rate skew (if telemetry is sampled) | The tuner scales counts by `1/sample_rate_applied` stored per-session. |

## Implementation Status

### Rust crates — shipped in `crates/plugins/format-pipeline/`

- [x] **L0 content-hash dedup** — `dedup.rs` (350 lines, 10 tests). SHA-256/128-bit fingerprint, LRU + partition-aware eviction, mutation-aware invalidation (`invalidate_file`), reference-hint renderer.
- [x] **Telemetry schema & sinks** — `telemetry.rs` (420 lines, 7 tests). `PipelineEvent` + `SessionSummary` + `TelemetrySink` trait (`JsonlSink` / `NullSink` / `MemorySink`). Thread-safe append-only JSONL with forward-compatible schema.
- [x] **AdaptiveConfig** — `adaptive_config.rs` (370 lines, 5 tests). TOML schema, 12 knobs across `dedup` / `templates` / `mckp` / `telemetry` / `endpoint_overrides`. Save/load with schema-version guard.
- [x] **Shape classifier** — `shape.rs` (400 lines, 10 tests). Rust port of the Python extractor; 11 shapes + 14 inner-format detectors (url / log / hash / diff / md-table / stack-trace / code-fence / …).
- [x] **L1 per-endpoint templates** — `templates.rs` (240 lines, 6 tests). `csv_from_md` (markdown → CSV), `pipeline_deep_mckp` (nested JSON compaction), `mr_diff_fence` (JSON `diffs[]` → markdown fences). Dispatcher: `apply_by_id`.
- [x] **L2 generic MCKP router** — `mckp_router.rs` (260 lines, 7 tests). Shape-dispatched format selection with threshold gating (min columns / min items / key stability / min fields) and `json_compact` fallback.
- [x] **LayeredPipeline orchestrator** — `layered_pipeline.rs` (420 lines, 9 tests). End-to-end L0 → L1 → L2 → L3 dispatch; `with_telemetry(sink)` emits one `PipelineEvent` per response; `on_compaction_boundary()` hooks into host's compact events.
- [x] **Criterion benchmark** — `benches/dedup_bench.rs`. Measured: `content_hash` 138 µs @ 64 KiB / 451 MiB/s, `cache.check(miss,lru=5)` = 2.7 ns, end-to-end (hash + check + insert) = 9 µs @ 4 KiB payload. p99 < 100 µs for 32 KiB.
- [x] **`devboy-tune` offline CLI** — `bin/tune.rs` (420 lines, 6 tests). `analyze` subcommand aggregates telemetry JSONL per endpoint, applies rules R1-R5, writes `pipeline_config.toml`. `show` pretty-prints a config.

Total: **242 tests** (233 lib + 6 bin + 3 doctests) passing; `cargo clippy --all-targets --all-features` clean.

### Python research scripts — shipped in `docs/research/scripts/`

- [x] **`extract_paper2_format_events.py`** — anonymizing extractor: scans `~/.claude/projects/*.jsonl`, emits `paper2_format_events.parquet` with shape, inner formats, content_sha256 prefix, file_path_hash, context_partition, endpoint_class. Never stores raw text.
- [x] **`simulate_paper2_pipeline.py`** — replay simulator: processes events in order, applies the 4-layer pipeline with tunable LRU, validates end-to-end savings on real corpus.
- [x] **Mined rules** — `docs/research/data/paper2_format_rules_v2.{yaml,csv}` (aggregates only).

### Validation snapshot on our corpus

| Metric | Value |
|--------|------:|
| Events analyzed | 144,658 |
| Sessions | 1,529 |
| Corpus baseline tokens | 115.57 M |
| Saved tokens | **40.46 M (35.01%)** |
| L0 hit rate | 29.9% of events |
| Hint real cost (cl100k_base) | 8–11 tokens |
| Sessions with compactions | 9.4% |
| Hash collisions (128-bit SHA-256 prefix) | 0 |
| Pipeline-invariant violations | 0 |

### Deferred to follow-up work

- [ ] **MCP middleware integration** — wire `LayeredPipeline` + `DedupCache` per-session state into `devboy-mcp` request path (hook after tool call returns, before response streams to client).
- [ ] **LLM comprehension eval** — run 100 real conversations × {Sonnet 4.6 / Haiku 4.5 / GLM-4.6 / gpt-oss:20b / gemma4:26b} on the GPU server, measure accuracy delta vs full-response baseline (tracked separately — GPU-bound).
- [ ] **Near-dup (Type-2) hints** — `> [near-ref: tc_42, status: pending→success]`. Requires delta extraction and `delta_match` primitive. Projected extra yield: +3-5 pp on pipeline-polling endpoints.
- [ ] **Team- and provider-shared fingerprints** (§Deployment Patterns B/C). Requires shared-bucket protocol and k-anonymity enforcement.
- [ ] **Format round-trip correctness tests** — sample 50 L1/L2-encoded responses, verify that a downstream decoder recovers full information vs the raw baseline.

## Encoder Bug Postmortem (2026-04-25)

During end-to-end LLM-comprehension validation we discovered a class of
silent data-loss bugs in the original Python reference encoder. The bug
is structural — not a transcription mistake — and it has implications
for any format-adaptive system that picks a single best encoding for a
mixed-shape input. The Rust production encoder is partially affected;
the relevant fix is described below.

### Symptom

On `deep`-shape and `nested`-shape records, naive monolithic CSV /
Markdown encoding scored 8.3% and 33.3% accuracy respectively, vs.
98.2% / 100% for `json_compact`. A schema-explainer hint prefix added 0
percentage points of recovery. The cost / accuracy frontier looked
catastrophic for "compressed" formats.

### Root cause

`_find_main_homogeneous_array(obj)` walked every subtree, picked the
largest array of objects, and emitted **only that array** as a CSV /
Markdown table. Two layers of data loss followed:

1. **Wrapping object's top-level fields are dropped.** If the input is
   `{"company":"Acme","year":2026,"employees":[...]}`, the encoder
   emitted a table of `employees` and silently discarded `company` and
   `year`. Any question whose gold answer references the wrapper
   ("What company is this?", "What year was this snapshot taken?")
   becomes unanswerable — not because the LLM is weak, but because the
   data is no longer in the prompt.

2. **Nested fields inside array elements are dropped.** Inside the
   chosen array, the encoder filtered to *primitive-only* columns. For
   `orders=[{id, total, customer:{...}, items:[...]}, ...]`, the table
   carried only `id` and `total`. Questions about `customer.email` or
   `items` returned `null`.

The first defect cost roughly −30 p.p. accuracy in aggregate; the
second cost a further ~−1.9 p.p. on the (smaller) population of
nested-in-array questions.

### Fix — `mckp_v2`

The corrected encoder applies the same per-subtree dispatch but with two
guarantees:

- **Top-level non-array fields are pre-rendered** as `key: value` lines
  (nested values become inline JSON) above the main table.
- **The main table uses the union of keys across all elements**;
  nested-typed cells are emitted as inline JSON in their cell, never
  dropped.

In Rust, `templates::deep_mckp_with_inner_table` implements this and is
invoked from `mckp_router::try_deep_mckp` ahead of the compact-JSON
fallback. It returns `None` when the input is not an object wrapping a
homogeneous array, so the rest of the pipeline is unchanged.
`mckp_router::try_array_csv` was already correct — it has used union
of keys since PR #204 — so the bug was confined to the
`Shape::NestedObject` branch and to the Python research encoder.

### Validation

109-question evaluation across {`glm-5.1`, `gpt-oss:20b`, `gemma4:26b`}:

| Encoder | glm-5.1 | gpt-oss:20b | gemma4:26b | Tokens vs `json_compact` |
|---|---:|---:|---:|---:|
| `json_compact` baseline | 98.2% | 95.4% | 95.4% | — |
| `csv` (naive monolithic) | 66.7% | 64.8% | 64.8% | −5% |
| `markdown` (naive monolithic) | 66.7% | 64.8% | 64.8% | +23% |
| `mckp_v1` (lossy reference) | 96.3% | 94.5% | 94.5% | −25% |
| **`mckp_v2`** | **98.2%** | **95.4%** | **95.4%** | **−16%** |

After the fix, MCKP per-subtree matches the JSON baseline accuracy on all
three models while preserving a 16% token saving overall. Two flips
were observed (both `glm-5.1` deep-shape questions whose gold answer
referenced a previously-dropped nested field), zero regressions.

### Implications

1. **Any "format X reduces tokens by N%" claim must be paired with an
   accuracy measurement on the same prompts.** Naive monolithic CSV
   looks like a 5% token saving on this corpus and is in fact a
   −30 p.p. accuracy regression.
2. **Hint prefixes cannot recover dropped data.** The hint experiments
   (`chunk1` vs `hint1`) confirmed this empirically: a one-line schema
   explainer added 0 lift to CSV / Markdown accuracy because the
   information was no longer present in the prompt.
3. **Encoders should fail closed on data loss.** A useful regression
   test is: for each encoder, parse the encoded form back and compare
   the recovered key set to the input's key set. Any drop is a bug.
4. **Token-cost numbers are tokenizer-dependent and must be reported
   alongside the tokenizer.** Inline JSON cells cost +119% prompt
   tokens per question on `glm-5.1` (Anthropic-class) but ±0% on
   gpt-oss / gemma (BPE). Same encoder, same prompts.

The Rust fix and a regression test (`deep_mckp_inner_table_*`) ship in
`crates/plugins/format-pipeline/src/templates.rs`.

## Related Work

- TOON (devboy internal, Paper 1 baseline) — custom 3-level format
- LLMLingua (token dropping) — format-agnostic, loses structure
- RECOMP (extractive compression) — sentence-level, not structure-aware
- Markdown as LLM format: numerous prompting papers confirm LLMs prefer Markdown
- Multi-choice Knapsack Problem: Pisinger 1995, Kellerer et al. 2004
