# Tool Documentation

This document describes the structure of tool definitions in DevBoy.

---

## Overview

Tool definitions are derived from code-level configurations such as:

* `base_tool_definitions()` – defines tool metadata
* `supported_categories()` – defines which providers support which tool categories
* Enricher schemas – modify parameters per provider

---

## Tool Categories

| Category | Description                                  |
| -------- | -------------------------------------------- |
| api      | Provider integrations (GitHub, GitLab, etc.) |
| pipeline | Data processing tools                        |

---

## Tools (Example Structure)

| Tool Name    | Category | Description         | Parameters     |
| ------------ | -------- | ------------------- | -------------- |
| example_tool | api      | Example description | param1, param2 |

---

## Provider Support Matrix

| Provider | Supported Categories |
| -------- | -------------------- |
| GitHub   | api                  |
| GitLab   | api                  |
| ClickUp  | api                  |

---

## Parameter Schema

Tool parameters may vary depending on the provider.

| Tool         | Provider | Parameters  |
| ------------ | -------- | ----------- |
| example_tool | GitHub   | repo, owner |
| example_tool | GitLab   | project_id  |

---

## Future Work

* Automatically generate this document from:

  * `base_tool_definitions()`
  * `supported_categories()`
  * Enricher schemas

* Add CLI command:

```bash
devboy docs generate-tools
```

---

## Notes

This is a manual structure to standardize tool documentation.
Automation can be implemented in future iterations.
