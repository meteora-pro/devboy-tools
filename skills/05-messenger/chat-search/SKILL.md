---
name: chat-search
description: Search messenger history for messages mentioning a keyword, optionally scoped to one chat or a date window.
category: messenger
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "find slack message about"
  - "search messenger for"
  - "who mentioned X in slack"
  - "find the message where"
  - "search chat for"
tools:
  - get_messenger_chats
  - search_chat_messages
---

# chat-search

Locate one or more messages across the configured messenger (Slack today, additional providers as they come online) by keyword, optional chat scope, and optional time window. The skill returns a ranked list of hits — it does **not** condense a long conversation into a narrative; for that, use `chat-summary`.

## When to use

- The user asks "find the slack message where Alice mentioned the rollback", "search messenger for 'feature flag cutover'", or similar.
- Another skill (e.g. `solve-issue`) needs to cite the originating chat discussion before acting on a ticket.
- The user remembers a phrase but not the channel or the author.

## Procedure

### 1. Resolve a chat scope (only if the user named one)

If the user said something like "in #eng" or "in the deploys channel", resolve the humanised name to a `chat_id` first. Searching without a scope is also fine — it's just slower and noisier.

```bash
# Narrow by name — pick the best match from the returned list
devboy tools call get_messenger_chats '{"search": "eng", "limit": 10}'
```

Useful filters: `chat_type` (`direct` / `group` / `channel`), `include_inactive` (archived chats are hidden by default). Note: `cursor`-based pagination is accepted by the tool's argument schema today but the response is formatted as text and does not surface a `next_cursor` back to the caller — narrow the hit list with `search` / `limit` instead, or refine the query.

### 2. Run the search

`search_chat_messages` takes a free-text `query` and optional `chat_id`, plus a provider-timestamp `since` / `until` window when the user cares about recency:

```bash
# Global search
devboy tools call search_chat_messages '{"query": "rollback", "limit": 30}'

# Scoped to one chat
devboy tools call search_chat_messages '{
  "query": "feature flag cutover",
  "chat_id": "C0123456789",
  "limit": 50
}'

# Last week, any chat
devboy tools call search_chat_messages '{
  "query": "incident",
  "since": "1712448000.000000",
  "until": "1713052800.000000"
}'
```

`since` / `until` are provider-native timestamps (Slack passes them straight through — floating-point epoch seconds, as strings). If the user phrases a window in natural language ("yesterday", "last week"), convert to epoch seconds before calling.

### 3. Narrow a too-large result set

`search_chat_messages` returns at most `limit` hits per call (capped at 1000). The response is rendered as formatted text and **does not surface a `next_cursor` back to the caller** today, so cursor-based pagination is not actionable from the tool output. Instead of trying to page, narrow the query:

- Tighten `since` / `until` to a smaller window.
- Add distinguishing words to `query`.
- Scope with `chat_id` to the channel the user actually meant.
- Raise `limit` as a last resort, but the hit list gets harder to skim fast.

If cursor pagination becomes genuinely necessary for a user's workflow, that requires a tool-side change to expose the pagination metadata, not a skill-side workaround.

### 4. Rank and present

Before handing the hits back to the user:

- **Sort by recency.** Messengers default to relevance; users almost always want "the most recent mention" first. Override the order client-side.
- **One line per hit.** Channel (or DM partner) + author + date + a one-line excerpt (≤ 120 chars, collapse newlines).
- **Deduplicate threads.** If multiple hits belong to the same thread, show the root hit once with a count of matching replies.
- **Cite coordinates, not permalinks.** The unified `MessengerMessage` type does not include a `permalink` field, so do not promise jump-to-message URLs. Surface the `chat_id` + message `id` / `timestamp` instead — that's enough for the user to open the message directly in Slack (`slack://channel?team=…&id=<chat_id>&message=<ts>`) or paste into a helper script.

Example render:

```
#eng        alice   2026-04-15 14:02   "…rolling back v2.4.1, see incident-204…"  (C0123/ts=1712584920.010)
DM bob      bob     2026-04-14 09:31   "feature flag cutover is done on staging"
```

## Success criteria

- The hits all contain the query term (the provider did real matching, not a partial / fuzzy miss the skill silently tolerated).
- The list is ordered newest-first and limited to what the user asked for — no dumping 500 rows when the user said "find the message".
- Channel, author, and date are present for every hit; excerpts are truncated, not walls of text.

## Guardrails

- Messenger content is often confidential. Do not forward raw message bodies to external systems (trackers, PR descriptions, documentation) without the user explicitly asking for that forwarding.
- If the configured Slack token lacks the read scopes (`channels:history`, `groups:history`, `im:history`, `mpim:history`), search results will be incomplete or empty. When hits look suspiciously thin, run `devboy test slack` and check the "Missing scopes" line.

## Non-goals

- This skill does not summarise a long channel or catch the user up on a day — use `chat-summary` for that.
- It does not send or forward anything — use `notify` when the user wants to post.
