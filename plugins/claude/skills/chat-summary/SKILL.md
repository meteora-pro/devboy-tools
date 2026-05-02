---
name: chat-summary
description: Catch the user up on a channel or DM by summarising messages over a time window, chunk by chunk, into grouped bullet points.
category: messenger
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "summarise today's messages"
  - "what happened in #eng this week"
  - "catch me up on the deploy channel"
  - "summarise slack"
  - "summary of chat"
tools:
  - get_messenger_chats
  - get_chat_messages
---

# devboy-chat-summary

Produce a concise, grouped summary of what happened in a chat (channel, group, or DM) over a user-specified window. The skill pulls message history in pages, summarises each page, then merges the page-level summaries into a single grouped bullet list — it does **not** keyword-search across chats (that's `devboy-chat-search`) and it does **not** send anything (that's `devboy-notify`).

## When to use

- The user asks "catch me up on the deploy channel", "what did #eng discuss this morning?", or "summary of my DMs with Alice this week".
- Another skill needs a short narrative of what a channel has been talking about before making a decision.
- The user was offline and wants a quick briefing before opening the messenger UI.

## Procedure

### 1. Resolve the chat

If the user named the chat by a humanised handle (`#eng`, "the deploy channel", "DMs with Alice"), look up the `chat_id`:

```bash
devboy tools call get_messenger_chats '{"search": "eng", "limit": 10}'
# Pick the match whose name / topic clearly corresponds to what the user said.
```

If the user already handed over a `chat_id`, skip this step.

### 2. Convert the window to provider timestamps

`get_chat_messages` takes `since` / `until` as provider-native timestamps (for Slack: floating-point epoch seconds, as strings — e.g. `"1712448000.000000"`). Convert the user's natural-language window ("today", "this week", "since Monday") into those before calling. Default to **the last 24 hours** when the user does not name a window.

### 3. Pull messages in narrow slices — do not load the whole history

The tool returns formatted text and **does not surface a `next_cursor`** in the output today, so you cannot page by cursor from the tool result. Instead, split a large window into several narrow `since` / `until` calls and summarise each slice independently:

```bash
# Morning slice
devboy tools call get_chat_messages '{
  "chat_id": "C0123456789",
  "since": "1713052800.000000",
  "until": "1713091200.000000",
  "limit": 200
}'

# Afternoon slice
devboy tools call get_chat_messages '{
  "chat_id": "C0123456789",
  "since": "1713091200.000000",
  "until": "1713139200.000000",
  "limit": 200
}'
```

Cap `limit` at 200 per call to keep each response small enough to summarise cleanly. If a slice comes back close to the cap, halve the window and re-run. If a thread is referenced by `thread_id` and the user asked about a specific thread, pull its replies separately:

```bash
devboy tools call get_chat_messages '{"chat_id": "C0123456789", "thread_id": "1712450000.001500", "limit": 200}'
```

### 4. Summarise chunk by chunk, then merge

Rather than feeding the entire history into one summary prompt:

1. **Per page**, draft 3–6 bullet points: decisions, open questions, action items, notable quotes (with author handle).
2. **Merge** the page-level bullets into a single list, collapsing duplicates and merging threads that span pages.
3. **Group the final list by topic**, not by time. Suggested top-level groups: `Decisions`, `Action items`, `Open questions`, `Context / FYI`. Omit groups that have no bullets.
4. Keep each bullet to **one line**, prefixed with the author when it matters (`alice: shipped v2.4.2`). Cite threads by their `chat_id` + `thread_id` pair — the unified messenger output does not supply a permalink field.

### 5. Render the summary

Target shape (adjust headings based on what actually came up):

```
Summary of #eng — 2026-04-14 00:00 UTC → 2026-04-15 00:00 UTC (312 messages, 4 threads)

Decisions
- Rolled back v2.4.1 after staging regression (alice, bob)
- Ship v2.4.2 with the patched migration tomorrow at 09:00 UTC

Action items
- @alice: draft incident-204 post-mortem by Thursday
- @carol: add regression test covering the migration case

Open questions
- Is the feature flag cutover still on for Friday?

Context
- Customer X asked about the GitLab SAML change — redirected to #support
```

If the window had fewer than ~20 messages, skip the grouping and render a single flat list.

## Success criteria

- The summary covers the requested window — the earliest and latest messages are reflected.
- Bullets are grouped, not a flat timestamp dump. Duplicates across threads are collapsed.
- Author handles are present where they add signal (decisions, action items).
- Pagination was actually used — the skill did not truncate silently at the first page.

## Guardrails

- Messages frequently reference confidential business decisions, customer names, or unreleased work. **Never forward the raw message content** to external systems — the issue tracker, a PR description, an email, a public channel — without the user's explicit ask. A summary surfaced in the conversation with the requesting user is fine; cross-posting it is not.
- If the Slack token lacks `channels:history`, `groups:history`, `im:history`, or `mpim:history`, the corresponding chat types will return empty. When a summary feels suspiciously short, run `devboy test slack` and check the "Missing scopes" line before blaming the users.

## Non-goals

- This skill does not search across many chats for a keyword — that is `devboy-chat-search`.
- It does not post the summary anywhere — if the user wants that, hand the summary to `devboy-notify` after confirming the target.
