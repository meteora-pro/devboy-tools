---
title: Research
description: Four research papers grounding the DevBoy tools format pipeline in a 144k-event production corpus.
---

# Research

Every non-trivial optimisation in the format pipeline is grounded in a paper measured against a real corpus — **523 Claude Code sessions, 10,644 MCP tool responses** from production traffic. The full source-of-truth (methods, datasets, reproducibility scripts) lives at [`docs/research/INDEX.md`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/INDEX.md) in the repo. This page is a quick orientation.

## Papers

| # | Paper | Status | Headline result |
|---|-------|--------|-----------------|
| [1](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/paper-1-trimtree.md) | TrimTree: priority-driven pagination — binary knapsack within a token budget, `p₁` metric | draft (820-line full draft, all experiments complete) | **3.3× p₁** vs uniform on power-law data; FIFO baseline 35% **replicated across 3 corpora**; KV-cache pass on Sonnet 4.5 ≈ **40% input-side savings** (66.5% hit rate) |
| [2](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/paper-2-mckp-format-adaptive.md) | Format-adaptive tree encoding — multi-choice knapsack picking CSV / table / key:value per subtree | draft | Per-call savings on the corpus: **avg 69% on `get_issues`** (top 92%), **avg 26% on `*_pipeline`**; ≥ 20% bucket hits 1.25% of all events but most calls of the shape-friendly endpoints |
| 3 ([theory](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/paper-3-context-enrichment.md) · [implementation](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/paper-3-tool-aware-enrichment.md)) | Context Enrichment Hypothesis + tool-aware knapsack with provider value models | draft (prefetch dispatcher merged in v0.22; production telemetry pending) | **Pearson r = −0.280** between `chars_per_item` and follow-up enrichment calls; thin issues (< 200 chars/item) → **43%** of turns add a `get_issue`; rich (1.5 k–4 k) → **2%** |
| [4](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/paper-4-notebook-parquet.md) | Dataset-as-context — large responses become queryable Parquet artefacts the LLM pulls from | draft (early concept, no measurements yet) | Hypothesised **60–80% additional** savings on top of TrimTree; evaluation harness not yet built |

## Corpus baselines

These numbers ground every paper above (paper 1 §B):

- `get_merge_request_diffs` — P90 = 35 k chars ≈ 10 k tokens, **28%** of responses exceed an 8 k-token budget
- `get_epics` — P90 = 43 k chars ≈ 12 k tokens, 37% exceed budget
- After overflow, agents always produce a text response on the next turn — they **never** retry / paginate

## Status

- **Paper 1** — complete draft, replicated across 3 corpora, lands in the next minor version.
- **Paper 2** — complete draft, lands in the next minor version (the format-adaptive encoder is already in the codebase under feature flag).
- **Paper 3** — prefetch dispatcher merged in v0.22; production telemetry pending.
- **Paper 4** — concept stage, no production code yet.

## Notebooks & data

Reproducibility scripts and the corpus notebook are under [`docs/research/`](https://github.com/meteora-pro/devboy-tools/tree/main/docs/research) in the repo (paper-1-repro, benchmarks, data, notebooks).
