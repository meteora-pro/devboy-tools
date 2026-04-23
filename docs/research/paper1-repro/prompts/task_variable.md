# Task-variable prompt template (NOT cached)

<!--
This portion is sent WITHOUT cache_control — it varies per task.
Keep the wrapper minimal so the LLM spends its attention on issue + candidates.
-->

TASK
====

Issue:
{{ISSUE_TEXT}}

Candidate files ({{N_ITEMS}} total, compressed under budget={{BUDGET_TOK}} tokens):
{{CANDIDATE_LIST}}

{{CHUNK_HINT}}

Return the single JSON object as specified in the system prompt. Nothing else.
