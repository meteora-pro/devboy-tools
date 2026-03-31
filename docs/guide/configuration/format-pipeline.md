# Format Pipeline Configuration

The format pipeline works out of the box with sensible defaults. All configuration is optional and goes into your `.devboy.toml` file.

## Quick Start

No configuration needed. The pipeline uses TOON format with 8,000 token budget by default.

Typical savings on real projects (kubernetes, vscode, rust-lang, golang):
- **TOON Full**: 3-17% fewer tokens than JSON
- **TOON Standard** (with budget trimming): ~44% savings
- **TOON Minimal** (with budget trimming): ~92% savings

Run `devboy benchmark --owner <owner> --repo <repo>` to measure savings on your project.

See [Format Pipeline Architecture](/guide/architecture/format-pipeline) for detailed benchmarks.

## Full Configuration Reference

```toml
[format_pipeline]
# Maximum token budget per tool response (default: 8000)
# ~6% of a 128K context window.
# Increase for projects with large issues/MRs, decrease for smaller contexts.
budget_tokens = 8000

# Safety margin for token estimation inaccuracy (default: 0.20)
# Covers up to 25% deviation in compression ratio after trimming.
# Increase if you see outputs slightly exceeding budget.
margin = 0.20

# Maximum trim-encode-verify iterations (default: 3)
# 2 is sufficient in 99% of cases; 3 is a safety net.
max_iterations = 3

# Default output format: "toon" or "json" (default: "toon")
# toon — optimized for LLM consumption (39-90% token savings)
# json — for programmatic processing
default_format = "toon"

# Override trimming strategies for specific tools.
# Keys are tool names, values are strategy names.
#
# Available strategies:
#   element_count      — for flat lists (issues, MRs)
#   cascading          — chronological decay for comments
#   size_proportional  — for diffs (weighted by file type)
#   thread_level       — for discussions (resolved vs unresolved)
#   head_tail          — for logs (head + tail preservation)
#   default            — uniform value, no semantic trimming
[format_pipeline.strategies]
# Examples:
# get_issues = "element_count"          # already the default
# get_job_logs = "head_tail"            # already the default
# "cloud__get_tasks" = "element_count"  # proxy tool override

# Automatic strategy mapping for proxy tools.
[format_pipeline.proxy_matching]
# When true (default), proxy tool names are stripped of their prefix
# (e.g. "cloud__get_issues" → "get_issues") and matched against
# hardcoded defaults. Set to false to require explicit strategy
# mapping for all proxy tools.
enabled = true
```

## Default Strategy Mapping

These built-in mappings are applied automatically:

| Tool Name | Strategy | Rationale |
|-----------|----------|-----------|
| `get_issues` | `element_count` | Flat list, decreasing value by position |
| `get_issue_comments` | `cascading` | Newest comments most valuable |
| `get_merge_requests` | `element_count` | Flat list, same as issues |
| `get_merge_request_diffs` | `size_proportional` | Lock files low priority, source high |
| `get_merge_request_discussions` | `thread_level` | Unresolved discussions prioritized |
| `get_job_logs` | `head_tail` | Config at start, errors at end |
| `get_pipeline` | `default` | Single object, minimal trimming |
| `get_users` | `default` | Simple flat list |
| `get_statuses` | `default` | Simple flat list |

## Proxy Tools

When proxy tools (from upstream MCP servers) are used, the pipeline automatically strips the prefix to find a matching strategy:

```
cloud__get_issues      →  get_issues      →  element_count
jira_proxy__get_tasks  →  get_tasks       →  default (no match)
```

You can override this by explicitly mapping proxy tool names:

```toml
[format_pipeline.strategies]
"jira_proxy__get_tasks" = "element_count"
```

Or disable automatic matching entirely:

```toml
[format_pipeline.proxy_matching]
enabled = false
```

## Common Scenarios

### Increase budget for large projects

If your project has many issues/MRs and you want more data per response:

```toml
[format_pipeline]
budget_tokens = 16000
```

### Use JSON for CI/CD integration

If you're processing tool output programmatically:

```toml
[format_pipeline]
default_format = "json"
```

### Custom strategy for a proxy tool

If you have a proxy tool that returns issue-like data:

```toml
[format_pipeline.strategies]
"myserver__search_tickets" = "element_count"
```

### Disable budget trimming

Set a very high budget to effectively disable trimming:

```toml
[format_pipeline]
budget_tokens = 1000000
```

## Chunk-Based Behavior

When tool output exceeds the budget, the pipeline automatically splits the response into chunks. The first response includes chunk 1 (highest-value items based on the active trimming strategy) and a chunk index describing all available chunks. Agents use `offset` and `limit` parameters in subsequent tool calls to fetch specific chunks on demand, without needing to read all data sequentially.

See [Format Pipeline Architecture — Chunk-Based Lazy Loading](/guide/architecture/format-pipeline#chunk-based-lazy-loading) for details on the chunk index format and data flow.

## Provider Result Metadata

When providers return list data, pagination and sort metadata from the upstream API (e.g., GitLab `X-Total` headers, Jira `total`/`startAt`/`maxResults`) is captured in `ProviderResult<T>` and flows through to `FormatMetadata`. This allows agents to understand the total dataset size and available sort options without additional API calls.

See [Format Pipeline Architecture — Provider Metadata](/guide/architecture/format-pipeline#provider-metadata) for details.
