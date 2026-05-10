# Loading Jira metadata at runtime

When you build an MCP server (or any tool surface) around the
`devboy-jira` crate, the [`JiraSchemaEnricher`] needs a
[`JiraMetadata`] cache to turn `customFields` / `priority` /
`components` / `linkType` from raw `customfield_*` ids and free
strings into typed parameters the LLM can pick from.

For downstream consumers who don't already have metadata loaded
out-of-band, [`JiraClient::load_default_metadata`] handles project
discovery and per-project metadata fetches in one call. You pick a
strategy, the method does the round-trips, and you feed the result
straight into the enricher.

## Strategies

```rust
pub enum MetadataLoadStrategy {
    Configured(Vec<String>),
    MyProjects,
    RecentActivity { days: u32 },
    All,
}
```

| Strategy | Use when | Round-trips per project (typical) | Over-cap behaviour |
|---|---|---|---|
| `Configured(keys)` | You already know the projects (env var, config file, allowlist). Skips Jira-side discovery entirely. | 5 (project + components + priority + linkType + field) | Truncate + `tracing::warn!`. The list came from a human; silently dropping is friendlier than erroring. |
| `MyProjects` | First-run default when the user hasn't picked a list. Jira returns the projects they've touched most recently. | 1 (discovery) + 5 per project | Discovery already caps at `MAX_ENRICHMENT_PROJECTS` via the `recent=N` parameter. |
| `RecentActivity { days }` | Cover active projects beyond what one user has touched — collaborative workspaces, oncall rotations. Uses a JQL search for issues updated in the last `days`. | 1 search (max 100 issues, deduped to project keys) + 5 per project | Hard cap at `MAX_ENRICHMENT_PROJECTS` after dedupe; freshest activity wins. |
| `All` | Small instances where you genuinely want every project. | 1 discovery + 5 per project | Hard error with a hint listing the other three strategies. `All` semantically means "give me everything"; silently truncating would hide projects the caller asked for. |

`MAX_ENRICHMENT_PROJECTS` is currently `30`. Beyond that the schema
gets fat enough to cost real tokens at every `tools/list`, and
agent budgets erode. The cap is enforced at two layers:

- `load_default_metadata` honours it per the table above.
- The schema enricher walks the first 30 projects (sorted by
  project key for determinism across reloads) when building both
  the flat customfield union and the per-name conflict groups.
  Both code paths emit a `tracing::warn!` when the cache carries
  more than 30 projects. Selecting the right 30 is the caller's
  responsibility — see the strategies above.

## Example

```rust
use devboy_jira::{JiraClient, JiraSchemaEnricher};
use devboy_jira::metadata::MetadataLoadStrategy;
use secrecy::SecretString;

let client = JiraClient::new(
    "https://example.atlassian.net",
    "PLATFORM",
    "alice@example.com",
    SecretString::from("…".to_string()),
);

// Pull the metadata for the projects the authed user touched most
// recently. The first call hits Jira; subsequent enricher calls
// read from the in-process cache.
let metadata = client
    .load_default_metadata(MetadataLoadStrategy::MyProjects)
    .await?;

// Plug it into the enricher you hand to your MCP / executor
// integration.
let enricher = JiraSchemaEnricher::new(metadata);
```

After this, `tools/list` responses for `create_issue` /
`update_issue` will surface:

- `epicKey`, `sprintId`, `epicName` typed aliases for the
  well-known agile customfields (when present on the instance).
- `cf_<sanitized_name>` slots for every other customfield in the
  union.
- An `anyOf` schema when the same display name resolves to
  different shapes across projects (see [Consuming tool
  schemas](./consuming-tool-schemas.md) for the wire shape).

## Caching the metadata

`load_default_metadata` doesn't memoise the result — calling it
twice fetches twice. If your runtime hydrates metadata on a
schedule (e.g. once per session, or every few minutes), wrap the
call in your own cache. The output of `load_default_metadata` is
`Clone + Serialize`, so storing it in an `Arc<JiraMetadata>` or
serialising it to disk is straightforward.

## See also

- [Consuming tool schemas](./consuming-tool-schemas.md) — how
  downstream consumers read the enricher's output via
  `base_tool_definitions()` and JSON Schema.
- [Tool reference](./tools.md) — the static catalogue, unenriched.

[`JiraClient::load_default_metadata`]: https://docs.rs/devboy-jira/latest/devboy_jira/struct.JiraClient.html
[`JiraMetadata`]: https://docs.rs/devboy-jira/latest/devboy_jira/struct.JiraMetadata.html
[`JiraSchemaEnricher`]: https://docs.rs/devboy-jira/latest/devboy_jira/struct.JiraSchemaEnricher.html
