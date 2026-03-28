# Enrichers

Enrichers dynamically modify MCP tool schemas and transform arguments at runtime. They implement the `ToolEnricher` trait from `devboy-core`.

## How It Works

```
tools/list request
    ↓
Executor collects base tool schemas
    ↓
Each enricher modifies schemas:
  - Remove unsupported params (GitHub: no priority)
  - Add enum values from metadata (ClickUp: status list)
  - Replace customFields with typed cf_* params (Jira)
    ↓
Return enriched schemas to client

tools/call request
    ↓
Each enricher transforms args:
  - cf_story_points: 5 → customFields: { "customfield_10001": 5 }
  - priority: "urgent" → priority: 1 (ClickUp numeric)
    ↓
Provider receives normalized args
```

## ToolEnricher Trait

```rust
pub trait ToolEnricher: Send + Sync {
    /// Which tools this enricher applies to.
    fn supported_tools(&self) -> &[&str];

    /// Modify schema during tools/list.
    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema);

    /// Transform args before execution.
    fn transform_args(&self, tool_name: &str, args: &mut Value);
}
```

## Built-in Enrichers

### Static Enrichers (no metadata needed)

**GitLabSchemaEnricher** — removes params not supported by GitLab:
- Removes: `priority`, `parentId`, `customFields`, `issueType`, `components`, `projectId`, `points`
- Adds: `link_type` enum `["relates_to", "blocks", "is_blocked_by"]`

**GitHubSchemaEnricher** — removes params not supported by GitHub:
- Same removals as GitLab
- `link_issues` tool marked as unsupported
- `transform_args`: maps `line_type` → GitHub `side` parameter (old→LEFT, new→RIGHT)

### Dynamic Enrichers (require metadata)

**ClickUpSchemaEnricher** — adapts tools using list metadata:
- Adds `status` enum from list statuses
- Adds `priority` enum `["urgent", "high", "normal", "low"]`
- Replaces `customFields` with individual `cf_*` params from custom field definitions
- `transform_args`: maps `cf_*` back to ClickUp format (dropdown name→orderindex, labels name→id)
- `transform_args`: maps priority name→numeric (urgent→1, high→2, normal→3, low→4)

**JiraSchemaEnricher** — adapts tools using project metadata:
- Adds `issueType` enum from project issue types (excludes subtask types)
- Adds `priority` enum from project priorities with alias hints
- Adds `components` enum from project components
- Single project: removes `projectId`, replaces `customFields` with `cf_*`
- Multi-project: `projectId` becomes enum of project keys, keeps generic `customFields`
- `transform_args`: maps priority aliases (urgent→Highest, high→High, etc.)
- `transform_args`: maps `cf_*` to Jira format (option name→`{id}`, array names→`[{id}]`)

### Pipeline Enrichers

**PipelineFormatEnricher** — adds `format` parameter to list tools:
- Adds `format` enum `["markdown", "compact", "json"]` to read tools
- Default: `markdown`

## Custom Fields

ClickUp and Jira support custom fields with different value transformation semantics:

### ClickUp Field Types

| Type | Input | API Format |
|------|-------|------------|
| `dropdown` | Option name (string) | orderindex (number) |
| `labels` | Name array | ID array |
| `number`, `currency` | Number | Pass-through |
| `checkbox` | Boolean | Pass-through |
| `date` | ISO 8601 string | Pass-through |
| `text`, `email`, `url`, `phone` | String | Pass-through |

### Jira Field Types

| Type | Input | API Format |
|------|-------|------------|
| `option` | Option name (string) | `{ "id": "option_id" }` |
| `array` | Name array | `[{ "id": "id1" }, ...]` |
| `number` | Number | Pass-through |
| `date` | YYYY-MM-DD | Pass-through |
| `datetime` | ISO 8601 | Pass-through |
| `string`, `any` | String | Pass-through |

## Creating a Custom Enricher

```rust
use devboy_core::{ToolEnricher, ToolSchema};
use serde_json::Value;

pub struct MyEnricher {
    allowed_statuses: Vec<String>,
}

impl ToolEnricher for MyEnricher {
    fn supported_tools(&self) -> &[&str] {
        &["get_issues", "create_issue"]
    }

    fn enrich_schema(&self, _tool: &str, schema: &mut ToolSchema) {
        schema.set_enum("status", &self.allowed_statuses);
    }

    fn transform_args(&self, _tool: &str, _args: &mut Value) {
        // No transformation needed
    }
}
```

Register with the executor:

```rust
executor.add_enricher(Box::new(MyEnricher {
    allowed_statuses: vec!["open".into(), "closed".into()],
}));
```
