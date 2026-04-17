---
id: ADR-010
title: Asset management — file attachments for AI agents
status: accepted
date: 2026-04-10
deciders: ["Andrei Mazniak"]
tags: ["rust", "assets", "files", "mcp", "cache", "pipeline"]
supersedes: null
superseded_by: null
---

# ADR-010: Asset management

## Status

**accepted** — Phases 1–3 shipped. Phase 5 (semantic `analyze_asset` via a configurable LLM) is still proposed and not implemented. Per-provider coverage of attachment download/delete is partial (see the Provider-specific support table below).

**What's shipped today:**

- `crates/devboy-assets/` exists with `cache`, `config`, `error`, `index`, `manager`, `rotation` modules. `AssetManager` is the public entry point, backed by `CacheManager` + `AssetIndex` with LRU rotation.
- Shared types in `devboy-core::asset` — `AssetContext`, `AssetContextKind`, `AssetMeta`, `AssetInput`, `AssetAnalysis`, `AssetCapabilities`, `ContextCapabilities`, `ContentKind`, `SemanticAnalysis`, plus markdown-attachment helpers (`MarkdownAttachment`, `parse_markdown_attachments`, `filename_from_url`).
- MCP tools `get_assets`, `upload_asset`, `download_asset`, `delete_asset` are wired in `devboy-executor`.
- Attachment upload is implemented on the issue provider for ClickUp, GitLab, and Jira. GitHub still returns `ProviderUnsupported` because its public API does not expose a direct upload path for issues / PRs.

**What's still proposed:**

- `replace_asset` tool
- `analyze_asset` tool and the Level-3 semantic pipeline (LLM provider abstraction, prompt templates, response cache)
- Complete attachment coverage across all providers — `get_issue_attachments` / `download_attachment` / per-context deletes still rely on trait-level `ProviderUnsupported` defaults for several providers

## Context

AI agents working with issue trackers and merge/pull requests need to:

1. **Attach files** — screenshots, logs, configs, video — to issues, comments, MRs
2. **Read and analyse** existing attachments from tickets and discussions
3. **Transfer files across contexts** — from a chat into an issue, from an MR into a ticket

### Current state

- `upload_attachment()` on `IssueProvider` is implemented for **ClickUp, GitLab, and Jira**. GitHub still returns `ProviderUnsupported` because the public API does not expose a direct upload surface for issues / PRs.
- `add_issue_comment` supports attachments via base64 → multipart upload → markdown URL.
- `get_issue_attachments` / `download_attachment` have trait-level defaults returning `ProviderUnsupported`; per-provider implementations are being rolled out.
- There is still no unified `Asset` abstraction exposed to agents — each provider has its own shape, and there is no local cache layer stitching them together.

### Where assets appear

| Context | Upload | Download | Providers |
|---------|--------|----------|-----------|
| Issue comment | Bug screenshot, log | Read attachment | ClickUp, Jira, GitLab, GitHub |
| Issue description | Config, spec | Fetch attachment | ClickUp, Jira |
| MR/PR comment | UI screenshot | Download artefact | GitLab, GitHub |
| MR/PR description | Demo video | — | GitLab, GitHub |
| Messenger (future) | Log file | Files from chat | Slack, Telegram |
| Knowledge base (future) | Diagram, PDF | Page attachments | Confluence |

### Provider constraints

- **GitLab** — no direct attachment API for comments. Workaround: `POST /projects/:id/uploads` uploads to the project and returns a markdown link `![image](/uploads/hash/file.png)` that is then embedded in issue/MR body or notes.
- **GitHub** — no public API for attachments in comments. Releases assets and Gists are possible workarounds.
- **Jira** — full support via `POST /rest/api/3/issue/{id}/attachments` on Jira Cloud, and `POST /rest/api/2/issue/{id}/attachments` on Self-Hosted / Data Center.
- **ClickUp** — full support via task attachments API (already implemented for upload).

## Decision

> **Decision:** Introduce an asset-management subsystem as a new crate `devboy-assets` with a local file cache, LRU rotation, and a plugin-based pipeline for analysis. Extend the existing provider traits with optional attachment CRUD methods that default to `ProviderUnsupported`.

### 1. Architecture

```
┌─────────────────────────────────────────────┐
│              MCP Tools Layer                 │
│  upload_asset  get_assets  download_asset    │
│  delete_asset  replace_asset  analyze_asset  │
└──────────────┬──────────────────────────────┘
               │
┌──────────────▼──────────────────────────────┐
│       Asset Manager (devboy-assets)          │
│  ┌────────┐ ┌──────────┐ ┌───────────────┐ │
│  │ Cache  │ │ Rotation │ │   Processor   │ │
│  │Manager │ │  (LRU)   │ │   Pipeline    │ │
│  └────┬───┘ └────┬─────┘ └───────┬───────┘ │
│       │          │               │          │
│  ~/.devboy/   config.toml     [plugins]    │
│    assets/                                  │
└──────────────┬──────────────────────────────┘
               │
┌──────────────▼──────────────────────────────┐
│       Provider Layer (ADR-004 traits)        │
│  GitLab: project uploads API                 │
│  GitHub: releases assets / markdown parsing  │
│  ClickUp: task attachments API               │
│  Jira: issue attachments API                 │
│  Slack: files.upload API (future)            │
└─────────────────────────────────────────────┘
```

### 2. Local file cache

The MCP server is a long-running process. Downloaded files are cached locally to avoid re-downloading on every access.

```
~/.devboy/assets/
├── index.json                 # asset_id → path, meta, last_accessed
├── issues/
│   ├── ISSUE-123/
│   │   ├── screenshot.png
│   │   └── error-log.txt
│   └── ISSUE-456/
│       └── config.yaml
├── merge-requests/
│   └── mr-374/
│       └── ui-screenshot.png
└── messages/
    └── slack-C04XXXXX/
        └── dump.log
```

`index.json` shape:

```json
{
  "version": 1,
  "assets": {
    "asset_abc123": {
      "provider": "clickup",
      "context": { "type": "issue", "key": "ISSUE-123" },
      "filename": "screenshot.png",
      "mime_type": "image/png",
      "size": 245000,
      "local_path": "issues/ISSUE-123/screenshot.png",
      "remote_url": "https://attachments.clickup.com/...",
      "downloaded_at": "2026-04-10T12:00:00Z",
      "last_accessed": "2026-04-10T14:30:00Z",
      "checksum_sha256": "abc123..."
    }
  }
}
```

### 3. LRU eviction

Configurable through the standard config entry points — `.devboy.toml` at the project root or `~/.devboy/config.toml` for the global default:

```toml
[assets]
cache_dir = "~/.devboy/assets"
max_cache_size = "1Gi"
max_file_age = "7d"
eviction_policy = "lru"     # lru | fifo | none
```

Eviction rules:

- Runs at MCP server start
- Runs before each download if the cache is near the size limit
- Runs periodically (every 30 minutes)
- `last_accessed` is tracked in `index.json` (not filesystem `atime`, which is unreliable on many setups)
- Priority for eviction: large binary files (video) evicted before small text files

### 4. Three-level processor pipeline

Content is analysed without loading raw bytes into the main conversation context. Three levels, with increasing cost:

| Level | What it does | Cost | When |
|-------|--------------|------|------|
| **L1: Metadata** | MIME, size, dimensions, line count | ~0 ms, no LLM | Automatic at download |
| **L2: Heuristics** | Regex for ERROR/WARN, last N lines, schema validity | ~1 ms, no LLM | Automatic at download |
| **L3: Semantic** | LLM-assisted: screenshot description, log root cause | ~1–10 s, one LLM call | On agent request |

#### L1–L2: built-in processors (no LLM)

```rust
#[async_trait]
pub trait AssetProcessor: Send + Sync {
    fn supported_types(&self) -> &[&str];
    async fn process(&self, asset: &CachedAsset) -> Result<AssetAnalysis>;
}

pub struct AssetAnalysis {
    pub summary: String,
    pub content_kind: ContentKind,
    pub extractable_text: Option<String>,
    pub key_findings: Vec<String>,
    pub metadata: HashMap<String, Value>,
    pub semantic: Option<SemanticAnalysis>,  // Populated by L3 if run
}

pub enum ContentKind {
    Text, Image, Video, Document, Data, Binary,
}
```

| File type | Processor | Output |
|-----------|-----------|--------|
| `.log`, `.txt` | `TextProcessor` | Last N lines, grep ERROR/WARN, size |
| `.json`, `.yaml`, `.toml` | `ConfigProcessor` | Structure, key fields, validity |
| `.png`, `.jpg`, `.gif` | `ImageMetaProcessor` | Dimensions, format, size (no vision) |
| `.csv` | `TableProcessor` | Columns, row count, sample rows |
| `.pdf` | `PdfMetaProcessor` | Page count, title, size |
| other | `FallbackProcessor` | MIME type, size, magic bytes |

#### L3: semantic analysis via a configurable LLM

Rust calls an LLM HTTP endpoint directly. Configured in `devboy.toml`:

```toml
[assets.semantic]
provider = "anthropic"          # anthropic | openai
endpoint = "https://api.anthropic.com/v1/messages"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
max_tokens = 4096
max_input_file_size = "5Mi"
max_batch_size = 10
analysis_cache_ttl = "24h"

[assets.semantic.prompts]
image   = "Describe what you see in this screenshot in the context of the software issue."
log     = "Analyse this log. Identify errors, patterns, anomalies. Focus on root cause."
config  = "Review this configuration. Check for misconfigurations, security issues."
default = "Analyse this file and provide key findings relevant to the issue context."
```

Two API dialects, same subsystem:

- `provider = "anthropic"` → Anthropic Messages API (`/v1/messages`), multimodal content blocks
- `provider = "openai"` → OpenAI-compatible Chat Completions (`/chat/completions`) — also covers Ollama, LM Studio, Azure OpenAI, z.ai, etc.

If `[assets.semantic]` is absent, L3 is simply unavailable. L1 and L2 keep working — the agent gets metadata and heuristic findings, just not LLM-generated summaries.

#### Two delivery modes

Agents can choose how they receive files:

```
┌────────────────────────────────────────────────────┐
│ download_asset(id, mode: "passthrough")            │
│ → raw file returned (base64 / path)                │
│ → agent decides what to do with it                 │
│ → good for multimodal agents (e.g. Claude Vision)  │
└────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────┐
│ analyze_asset(ids, prompt, context)                │
│ → devboy ships files to the configured LLM         │
│ → returns a structured summary                     │
│ → agent only sees the summary, not raw bytes       │
│ → saves tokens in the main conversation context    │
└────────────────────────────────────────────────────┘
```

Both modes matter — a multimodal agent may want to look at a screenshot itself, but a 50 MB log is much better summarised by a separate LLM call than dumped into the conversation.

#### `analyze_asset` flow

```
analyze_asset(assets[], prompt, context)
    │
    1. Gather files from cache (download if missing)
    2. Run L1+L2 processors                → basic_analysis per file
    3. Check result cache: hash(sorted_checksums + prompt)
       → if hit, return cached result
    4. Build the LLM request:
       System: type-specific base prompt
       + "Context: issue X: …"
       + "Basic analysis: 3 ERRORs found in log…"
       User: agent's custom prompt
       Content: [file1, file2, …] (text or base64 image)
    5. HTTP POST → configured LLM endpoint
    6. Parse → SemanticAnalysis
    7. Cache result by hash(checksums + prompt)
    8. Return to agent
```

**Batch analysis** — several files in one LLM request for cross-file reasoning ("screenshot 2 shows error 500, which matches line 147 in error.log").

**Agent-supplied prompt** — the calling agent can narrow the analysis ("find Redis timeout errors in this log").

**Auto-enrichment** — if the files are bound to an issue/MR, that context (title, description, labels) is pulled from provider metadata and added to the prompt automatically.

#### Beyond files

`analyze_asset` can also accept non-file content — URLs or arbitrary text:

```json
{ "content": "https://example.com/long-report", "content_type": "url", "prompt": "Summarise key findings" }
```

```json
{ "content": "<very long JSON from get_merge_request_diffs>", "content_type": "text", "prompt": "What are the riskiest changes?" }
```

This turns `analyze_asset` into a generic **context-saving tool** — an agent can offload heavy content to a separate LLM call instead of blowing up its own context window.

### 5. Shared types

```rust
// crates/devboy-core/src/asset.rs
pub enum AssetContext {
    Issue { key: String },
    IssueComment { key: String, comment_id: String },
    MergeRequest { mr_id: String },
    MrComment { mr_id: String, note_id: String },
    Chat { chat_id: String, message_id: String },
    KbPage { page_id: String },
}

pub struct AssetMeta {
    pub id: String,
    pub filename: String,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
    pub url: Option<String>,
    pub created_at: Option<String>,
    pub author: Option<String>,
    pub cached: bool,
    pub local_path: Option<String>,
    pub analysis: Option<AssetAnalysis>,
}

pub struct AssetInput {
    pub filename: String,
    pub data: Vec<u8>,
    pub mime_type: Option<String>,
}
```

### 6. New MCP tools

| Tool | Purpose | Parameters |
|------|---------|------------|
| `upload_asset` | Upload a file to a context | `context`, `filename`, `data` (base64), `mime_type` |
| `get_assets` | List attachments for a context | `context` (issue/MR/comment) |
| `download_asset` | Fetch to cache or pass to agent | `asset_id` or `url`, `mode: "passthrough" \| "cache"` |
| `delete_asset` | Delete an attachment (when supported) | `context`, `asset_id` |
| `replace_asset` | Replace an attachment (`delete` + `upload`) | `context`, `old_asset_id`, `filename`, `data`, `mime_type` |
| `analyze_asset` | Semantic analysis via the LLM pipeline | `assets[]` or `content`, `prompt`, `context`, `depth` |

`delete_asset` and `replace_asset` are only available when the provider supports deletion for the given context. The agent sees this through `asset_capabilities` in the schema enrichment (see below) and does not attempt unsupported operations.

### 7. Capability enrichment

Two layers:

**Per-attachment, in tool responses** — for example, `get_issues` enriches each issue with:

```json
{
  "key": "ISSUE-123",
  "title": "UI broken on mobile",
  "attachments_count": 3,
  "attachments": [
    {
      "filename": "screenshot.png",
      "size": 245000,
      "mime_type": "image/png",
      "cached": true,
      "local_path": "~/.devboy/assets/issues/ISSUE-123/screenshot.png"
    }
  ]
}
```

**Per-provider, in the schema** — declared by the provider:

```json
{
  "asset_capabilities": {
    "issue":                 { "upload": true, "download": true, "delete": true,  "list": true, "max_file_size": 10485760, "allowed_types": ["image/*", "text/*", "application/pdf"] },
    "issue_comment":         { "upload": true, "download": true, "delete": false, "list": true },
    "merge_request":         { "upload": true, "download": true, "delete": false, "list": true },
    "merge_request_comment": { "upload": false, "download": true, "delete": false, "list": true }
  }
}
```

Example: Jira's enricher exposes `issue.delete = true`; ClickUp's exposes `issue.delete = false`. The agent sees this before making calls and adapts.

### 8. Provider trait extensions

Methods are added to the existing traits (rather than introducing a separate `AssetProvider` trait). All defaults return `ProviderUnsupported`:

```rust
// IssueProvider additions
async fn get_issue_attachments(&self, key: &str) -> Result<Vec<AssetMeta>> { unsupported() }
async fn download_attachment(&self, key: &str, asset_id: &str) -> Result<Vec<u8>> { unsupported() }
async fn delete_attachment(&self, key: &str, asset_id: &str) -> Result<()> { unsupported() }
fn asset_capabilities(&self) -> AssetCapabilities { AssetCapabilities::default() }

// MergeRequestProvider additions
async fn get_mr_attachments(&self, mr_id: &str) -> Result<Vec<AssetMeta>> { unsupported() }
async fn delete_mr_attachment(&self, mr_id: &str, asset_id: &str) -> Result<()> { unsupported() }
```

```rust
pub struct AssetCapabilities {
    pub issue:           ContextCapabilities,
    pub issue_comment:   ContextCapabilities,
    pub merge_request:   ContextCapabilities,
    pub mr_comment:      ContextCapabilities,
}

pub struct ContextCapabilities {
    pub upload: bool,
    pub download: bool,
    pub delete: bool,
    pub list: bool,
    pub max_file_size: Option<u64>,
    pub allowed_types: Option<Vec<String>>,
}
```

Each provider implements `asset_capabilities()` with its actual support. The enricher reads this and publishes it in the schema.

### 9. Provider-specific support

| Provider | Upload | Download | List | Delete | Notes |
|----------|--------|----------|------|--------|-------|
| **ClickUp** | ✅ (`POST /task/{id}/attachment`) | to add | to add | ❌ (no public API) | Upload shipped |
| **Jira** | ✅ (v3 on Cloud, v2 on Self-Hosted / Data Center) | to add | to add | to add | Full API available; delete is implementable |
| **GitLab** | ✅ (project uploads + markdown link) | to add (via URL) | to add (parse markdown) | partial (edit out of body/comment) | No physical-delete API; "delete" means "remove the markdown link" |
| **GitHub** | ❌ (no public API for issues/PRs) | to add (via URL) | to add (parse markdown) | partial | Release assets are a separate path if we need upload |

## Consequences

### Positive

- ✅ Agents can work with files end-to-end — read screenshots, analyse logs, attach artefacts
- ✅ Local cache speeds up repeated access; multimodal agents can read files from disk
- ✅ Plugin-based analysis keeps heavy content out of the main conversation context
- ✅ Enrichment gives LLMs metadata about files without forcing a download
- ✅ Unified API across providers — the agent writes the same code for ClickUp, Jira, GitLab

### Negative

- ❌ Extra complexity — a new crate, a cache, rotation, eviction
- ❌ GitLab/GitHub support is partial because their APIs are partial
- ❌ Binary files (video) can't be analysed directly by the LLM pipeline without external tools

### Risks

- **Cache corruption** — if `index.json` falls out of sync with on-disk files. Mitigation: integrity check at startup; rebuild the index if drift is detected.
- **Disk usage** — heavy use without a configured limit could fill a disk. Mitigation: default 1 GiB cap; warn at 80 %.
- **Malicious content** — downloaded files could be hostile. Mitigation: never execute downloaded files; validate MIME types before use.

## Alternatives Considered

### Alternative A: Separate `AssetProvider` trait

**Why rejected:** Upload/download is semantically tied to the context (issue, MR, comment). A separate trait would force an extra mapping layer. Extending the existing traits with optional methods that default to `ProviderUnsupported` is a cleaner fit.

### Alternative B: Always delegate analysis to the calling agent

**Why rejected:** Overloads the main conversation context. Large logs or many screenshots fill up the context window fast. A three-level pipeline gives us summarisation without context cost; `passthrough` is still available when the agent genuinely wants raw bytes.

### Alternative C: Only external type-specific HTTP processors (image_analyzer, document_summariser)

**Why rejected:** Requires running separate services and configuring endpoints per file type. A single configurable LLM endpoint with type-specific system prompts is simpler to configure and covers every file type.

### Alternative D (chosen): Three-level pipeline with a configurable LLM

**Why chosen:** Works out of the box without any LLM (L1–L2). L3 semantic analysis is optional and user-configured. Supports batch, custom prompts, auto-enrichment, and generalises beyond files to URLs and large tool responses.

## Implementation

### Phase 1 — core infrastructure

- New crate `crates/devboy-assets/` (cache manager, index, rotation)
- Shared types `AssetMeta`, `AssetContext`, `AssetInput` in `devboy-core`
- Configuration in `.devboy.toml`

### Phase 2 — provider implementations

- ClickUp: download + list attachments
- Jira: upload + download + list + delete
- GitLab: project uploads + markdown parsing
- GitHub: markdown parsing + download

### Phase 3 — MCP tools

- `upload_asset`, `get_assets`, `download_asset`
- Enrichment: attachment count + metadata in `get_issues` responses

### Phase 4 — L1/L2 processor pipeline

- Built-in processors (text, image-meta, config, table)
- Auto-run at download — metadata + heuristics without LLM

### Phase 5 — L3 semantic analysis

- `analyze_asset` MCP tool with batch, prompt, context parameters
- Configurable LLM endpoint via `[assets.semantic]`
- Result cache keyed on `hash(checksums + prompt)`
- Two delivery modes: passthrough and semantic
- Non-file inputs: URL analysis, large-response summarisation

Issues to track work are on GitHub under `meteora-pro/devboy-tools`.

## References

- [ClickUp Attachments API](https://clickup.com/api/clickupreference/operation/CreateTaskAttachment/)
- [Jira Attachments API](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issue-attachments/)
- [GitLab Project Uploads API](https://docs.gitlab.com/ee/api/projects.html#upload-a-file)
- [ADR-007: Plugin architecture](./ADR-007-plugin-architecture.md) — pipeline plugin hooks

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-10 | Andrei Mazniak | Initial version |
| 2026-04-11 | Andrei Mazniak | Three-level pipeline (metadata/heuristics/semantic), configurable LLM, batch analysis, passthrough mode, extension to URL/response inputs |
| 2026-04-11 | Andrei Mazniak | CRUD operations: `delete_asset`/`replace_asset`, per-context capabilities, `AssetCapabilities` in provider traits, schema enrichment |
| 2026-04-17 | Andrei Mazniak | Translated to English; removed cross-repo references; kept as `proposed` (partially implemented) |
| 2026-04-17 | Andrei Mazniak | Promoted status to `accepted` for phases 1–3 (devboy-assets crate, core asset types, MCP tools `get_assets`/`upload_asset`/`download_asset`); phase 5 (semantic `analyze_asset`) remains proposed |
| 2026-04-17 | Andrei Mazniak | Corrected provider status: `upload_attachment` is implemented for ClickUp, GitLab, and Jira (GitHub still unsupported). Jira supports REST v3 on Cloud and v2 on Self-Hosted |
| 2026-04-17 | Andrei Mazniak | Flipped frontmatter `status: proposed` → `accepted` so it matches the body's `## Status` section and the index. Shared-types snippet now points at `crates/devboy-core/src/asset.rs` (not `types.rs`) and uses the real `MergeRequest { mr_id }` variant. "What's shipped" lists `delete_asset` as wired (it lives in `devboy-executor`) and credits ClickUp/GitLab/Jira uploads. Asset config section now names `.devboy.toml` / `~/.devboy/config.toml` instead of a fictional `devboy.toml` |
