# Paper 3 — Corpus findings (P-3-01)

> Anonymised aggregates from 3 185 Claude Code sessions (≈ 258 000 tool calls).
> Data lives in `docs/research/data/paper3_*.csv` (k-anonymised, K = 5).
> Per-event records are deliberately kept out of the repository — they
> sit in `/tmp/claude_analysis/` and never ship.

## What the dataset shows in plain language

Every row in our corpus is one tool call by an LLM agent. We grouped
them into **patterns** — sequences that fire over and over across
different sessions. For each pattern we measured how often it
happens, how many tokens the response carries, and what the agent
typically does next. Below is the concrete list of patterns, why
they show up, and what the Paper 3 enricher does for each.

The enricher is the pre-flight planner that sits in front of the LLM:
it reads the user's intent + recent tool calls, looks up the
annotations attached to each tool, and either pre-fetches data, swaps
a heavy response for a hint, or restricts the result projection — all
before the LLM emits the next `tool_use`.

---

### 1. Pipeline polling (the one with the biggest visible savings)

Two MCP endpoints (`*__get_branch_pipeline`, `*__get_meeting_transcript`)
are called repeatedly for the same identifier — the agent is waiting
for an asynchronous job to finish. Median response is 4–5 kB and the
self-loop edges show up in the top of the list (`mcp__*__get_branch_pipeline
→ mcp__*__get_branch_pipeline`, several hundred occurrences each).

**What the enricher does.** Marks these tools with
`freshness_ttl_s ≈ 15` and `near_ref_enabled = true` (Paper 2
§Near-reference, already shipped in P-203-10). The second poll within
15 seconds returns `> [near-ref: tc_X, status: pending→success,
duration: +22s]` — about 18 tokens instead of 5 000. After three
identical polls in a row the planner refuses to re-issue the call at
all and simply emits `> [polling: <id>, attempts=3, no change]` so
the LLM stops asking.

### 2. File re-reads (the workhorse pattern)

`Read → Read` fires 22 243 times in the corpus — the agent re-reads
the same file (often by chunk for long files). Median response is
2.5 kB; p99 reaches 43 kB.

**What the enricher does.** This is exactly the L0 dedup case from
Paper 2. The annotation is `value_class = "critical"` (file content
is non-negotiable for code-edit work) but `freshness_ttl_s = 0`
because we rely on the **mutation hook** instead: any
`Edit / Write / MultiEdit` on the same path invalidates the cache
synchronously. The second `Read` of an unmodified file collapses to
`> [ref: tc_X, byte-identical]` — typically 9 tokens.

### 3. Find → fix → verify loop

The combined volume of `Grep → Edit` (1 120) + `Edit → Grep` (1 671)
+ `Edit → Bash` (8 161) + `Edit → Read` (4 388) shows the canonical
*"locate uses → patch them → run the tests"* loop. The agent rarely
holds the full Grep listing in mind; it greps again for the next match.

**What the enricher does.** Three things:

- After `Grep`, annotation `follow_up.likely_next = ["Read", "Edit"]`
  tells the planner to *prefetch* the top-3 hits as `Read` calls if
  budget permits — saves an entire LLM turn.
- `Grep` itself gets `value_class = "critical"` and a tight projection
  (file/line/match — drop surrounding context) because median
  responses are short (246 B) and almost always cited verbatim.
- After `Edit`, the planner registers `Bash` as the likely next call
  with `projection = none` (the agent picks the verify command). No
  speculation here — `Bash` outputs are too varied to prefetch.

### 4. Bulk listing → inspect-each pattern

`Glob → Read` (2 007) and `Glob → Glob` (2 540) — the agent narrows
in on a directory, then reads files one at a time. `Read → Glob`
(1 374) is the same in reverse: agent reads one file, then expands
its search.

**What the enricher does.** `Glob` annotation declares
`follow_up.likely_next = ["Read", "Grep"]`. When the user's intent
contains a phrase like *"where is X used"*, the planner upgrades
`Glob` to *speculative-prefetch mode*: it performs the glob and
**also** reads the top-N matches in the same response, packaging
them as a single multi-tool result. The LLM sees the file list **and**
the contents in one turn instead of two. Budget gate prevents this
from blowing the context window.

### 5. Web search → web fetch chain

`WebSearch → WebFetch` fires 1 081 times — the agent searches, then
fetches one of the top URLs. WebSearch carries 3 kB on average
(snippets + URLs); WebFetch carries 1–2 kB (the actual page).

**What the enricher does.** WebSearch annotation:
`follow_up.likely_next = ["WebFetch"]`, `projection.must_have =
["title", "url"]`, `projection.optional = ["snippet"]` (drop snippets
under tight budget). The planner can also **proactively fetch the top
URL** when the user's question is fact-finding — saves a round-trip.

### 6. Task management noise (don't waste budget on it)

`TaskCreate → TaskCreate` (2 210), `TaskUpdate → TaskUpdate` (2 278).
Each response is tiny — TaskUpdate's median is **23 bytes**, p99 is
36 bytes. These tools generate metadata about the agent's internal
plan, not knowledge.

**What the enricher does.** Annotation: `value_class = "audit_only"`.
The planner skips them in the knapsack accounting (no budget cost,
no value) but keeps them in the trace for telemetry. They never
trigger dedup — pointless when bodies are 23 bytes.

### 7. Long-poll task chains

`Bash → TaskUpdate` (1 447), `Edit → TaskUpdate` (600), and
`TaskUpdate → Bash` (1 209) — the agent updates its todo list after
every concrete step. The cost is negligible (audit_only) but the
ordering is informative.

**What the enricher does.** Uses these edges to **infer session
phase** for the agent profile resolver (Paper 2 `profiles.agent`):
high `* → TaskUpdate` density signals `marathon_refactor`, which
already lifts `recursion_depth` to 7 and turns on near-ref hints.

### 8. Self-search loops (a leak we should plug)

`ToolSearch → ToolSearch` (267) — the LLM searches for a tool, fails
to find a match, searches again with a different query. Median
response is **0 bytes** (no match in 50%+ of cases).

**What the enricher does.** This is a *negative* finding: the agent
is fishing. Annotation: `value_class = "supporting"`,
`fail_fast_after_n = 2` — after two empty `ToolSearch` calls in a
row, the planner emits `> [tool not found, last attempts: …]` and
the LLM stops searching for that name.

### 9. Browser automation chains

`mcp__*__browser_click → mcp__*__browser_click` (340) — long
sequences of UI clicks during browser automation. Average response
is 2 kB but p90 reaches 10 kB (full page snapshot after click).

**What the enricher does.** Annotation: `cost_model.typical_kb = 2,
max_kb = 10`, `projection.must_have = ["url", "title", "newly_visible"]`,
`projection.optional = ["full_dom"]` (DOM dumps are 90% boilerplate
and rarely cited). The planner caps DOM at 4 kB unless the user's
intent explicitly mentions HTML.

---

## How the enricher resolves a turn end-to-end

1. **Read intent** from the user / assistant messages and the recent
   tool history.
2. **Look up annotations** for every tool that *could* run next.
3. **Build a knapsack** of `(tool, projection) → (estimated_value,
   cost_tokens)` pairs.
4. **Solve**: maximise value subject to `cost ≤ budget_for_turn`,
   honouring `prereq_tools` closure.
5. **Emit a plan** — either as response-level hints the LLM consumes
   verbatim, or (when integrated into the MCP layer) as proactive
   pre-fetched tool results inserted into context.

The annotations themselves come from three layers:

- **Built-in defaults** shipped per provider crate — anchored on the
  numbers above.
- **`[tools.<name>]` overrides** in `~/.devboy/pipeline_config.toml`
  for site-specific tuning. Same TOML hot-reload path as Paper 2.
- **`tune from-claude-logs --tools`** — the offline analyser produces
  a starter `[tools.*]` block from the user's own session history,
  same workflow as Paper 2 `tune from-claude-logs`.

## Anonymity safeguards

- `paper3_followup_edges.csv` and `paper3_tool_volume.csv` are
  **aggregate-only** — no session id, no turn index, no user-level
  fingerprint.
- K-anonymity threshold K = 5: any edge or tool seen by fewer than
  five distinct sessions is dropped (319 tools and 6 319 edges fell
  below the threshold and are *not* in the committed files).
- MCP slugs were already replaced with a 6-hex hash by the
  extractor; only the public *verb* survives.
- Per-event parquets stay in `/tmp` and are excluded by the existing
  `docs/research/benchmarks/` gitignore rule.

## Numbers used elsewhere

| Tool | Calls | Median bytes | p90 bytes | Used as default for |
|---|---:|---:|---:|---|
| Bash | 110 930 | 223 | 1 772 | `cost_model.typical_kb = 0.2` |
| Read | 50 675 | 2 473 | 12 745 | `cost_model.typical_kb = 2.5`, `value_class = critical` |
| Edit | 30 781 | 162 | 991 | `cost_model.typical_kb = 0.2`, `invalidates = ["Read", "Grep"]` |
| Grep | 16 718 | 246 | 2 592 | `cost_model.typical_kb = 0.3` |
| Write | 6 654 | 137 | 181 | `cost_model.typical_kb = 0.2`, `invalidates = ["Read"]` |
| Glob | 6 202 | 156 | 4 077 | `cost_model.typical_kb = 0.2`, `follow_up = ["Read", "Grep"]` |
| TaskUpdate | 5 386 | 23 | 24 | `value_class = audit_only` |
| TodoWrite | 3 201 | 160 | 160 | `value_class = audit_only` |
| WebFetch | 1 825 | 1 145 | 1 836 | `cost_model.typical_kb = 1.2` |
| WebSearch | 1 674 | 3 100 | 3 896 | `cost_model.typical_kb = 3.1`, `follow_up = ["WebFetch"]` |
| Agent | 1 376 | 6 604 | 15 787 | `cost_model.typical_kb = 6.5`, `value_class = supporting` |

These are the anchors P-3-05 will pour into Rust source as the shipped
defaults. Users keep the right to override every one of them through
the same `[tools.*]` TOML overlay that Paper 2 introduced for the
encoder profiles.
