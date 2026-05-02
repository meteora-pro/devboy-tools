---
name: tools-catalog
description: Enumerate and introspect the active tool bundle — names, categories, schemas, how to invoke each tool from the CLI.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "list devboy tools"
  - "what tools does devboy have"
  - "show devboy tools"
  - "inspect devboy tools"
tools:
  - tools
  - config
---

# tools-catalog

Answer the question "what tools does the current `devboy-tools` installation expose, and how do I invoke them?". Useful as the first step of any exploration — other skills invoke tools through `devboy tools call`, and that assumes the agent knows the tool names and argument shapes.

## When to use

- First contact with a devboy setup — the agent wants to know what's available before writing a multi-tool recipe.
- Before writing a new skill — confirm the tool names and argument shapes you plan to use actually exist in the active configuration.
- To confirm that a configured provider's tools really show up (e.g. after `devboy-setup` or `devboy-repair`).

## Procedure

### 1. Enumerate

```bash
devboy tools list
```

Output is a flat list of tool names with enabled/disabled status — `devboy tools list` does not print descriptions or categories itself. Tools from providers that are not configured for the active context are filtered out, so an empty or short list almost always means "no provider is wired up yet" rather than "devboy is broken".

### 2. Inspect one tool's schema

`devboy tools call` does not take `--help`; the schema is not served as a flag. The only way to see a tool's full JSON Schema (required fields, enums, provider-specific `cf_*` custom-field extensions) is to ask the MCP server directly:

```bash
devboy mcp <<< '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | \
  jq '.result.tools[] | select(.name=="get_issues")'
```

The MCP server exits after serving one request on stdin, so no background process remains.

### 3. Invoke a tool

Synchronous call via the CLI:

```bash
# POSIX shells
devboy tools call get_issues '{"state": "open", "limit": 20}'

# Windows cmd.exe / PowerShell
devboy tools call get_issues "{\"state\": \"open\", \"limit\": 20}"
```

The default JSON payload is `{}`. For tools that take no arguments, omit it entirely:

```bash
devboy tools call list_contexts
```

### 4. Filter to a category

If you only care about issue-tracker tools, filter the list:

```bash
devboy tools list | grep -E "issue|comment"
```

(There is no built-in `--category` filter on `tools list` today — that is a follow-up.)

### 5. Disable unused tools

Skills that only need a handful of tools can disable the rest to shrink the prompt that an MCP client sees. This is optional — most skills leave the defaults alone.

```bash
devboy tools disable some_unused_tool another_one
devboy tools list          # confirm
devboy tools reset         # undo
```

## Key concepts worth narrating to the agent

- **Tools are provider-scoped.** A given install only exposes the tools for the providers that are configured. `devboy-tools-catalog` therefore gives a snapshot, not a universal list.
- **Categories are coarse.** `ToolCategory` has six members (`GitRepository`, `IssueTracker`, `Epics`, `Releases`, `MeetingNotes`, `Messenger`). A tool from an uncovered category is hidden by the executor.
- **Enrichment happens at `tools/list` time.** Each provider gets a chance to add provider-specific parameters to the schema (custom fields, enum values drawn from real project metadata). The schema you see in `tools/list` is already enriched — the agent does not need to flatten or merge anything.
- **Prefer `devboy tools call` over MCP** when a skill only needs one tool. It avoids paying the full `tools/list` context tax the MCP transport imposes.

## Success criteria

- The agent can name the tools that apply to the current configuration without guessing.
- For any tool the agent plans to call next, the argument shape matches the real schema.
- The agent knows whether a tool is shipped but disabled versus not available at all (different problems, different fixes).
