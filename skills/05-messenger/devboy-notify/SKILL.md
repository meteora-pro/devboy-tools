---
name: devboy-notify
description: Post a short, structured notification — subject, bullets, optional link — to a chat or channel the user explicitly names.
category: messenger
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "post to"
  - "notify the team"
  - "send a message to slack"
  - "announce in"
  - "ping the channel"
tools:
  - get_messenger_chats
  - send_message
---

# devboy-notify

Send a one-shot, structured notification — a short subject line, 2–3 body bullets, and an optional link — to a chat or channel the user has explicitly asked you to post in. The skill confirms the target and verifies the required write scope **before** calling `send_message`; it never posts speculatively.

## When to use

- The user says "post to #eng that the rollback is done", "notify the team about the release", or "send a quick message to #support".
- Another skill finished a long operation (deploy, migration, review sweep) and the user explicitly asked for a chat notification at the end of it.

## Preconditions

1. **The user must have asked.** If the user said something like "maybe we should tell #eng" or "should we notify?", treat that as a **draft** — confirm the target and the text with the user before calling `send_message`. Never post on an ambiguous signal.
2. **Slack write scope present.** `send_message` on Slack requires the `chat:write` scope. The full list of scopes the devboy Slack integration expects is defined by `default_slack_required_scopes` in `crates/devboy-core/src/config.rs` (`channels:read`, `channels:history`, `groups:read`, `groups:history`, `im:read`, `im:history`, `mpim:read`, `mpim:history`, `chat:write`, `users:read`). Verify the token has them before posting — see step 2 below.

## Procedure

### 1. Resolve the target chat

If the user named the chat by a handle (`#eng`, "the release channel", "DM to alice"), look up the `chat_id`:

```bash
devboy tools call get_messenger_chats '{"search": "eng", "limit": 10}'
```

Pick the match whose name clearly corresponds to what the user asked for. If more than one chat matches and the correct one is not obvious, **stop and ask** rather than guessing. Posting to the wrong channel is hard to undo.

### 2. Verify the write scope — fail fast if it's missing

Before the first `send_message` of a session, confirm the Slack token carries the required scopes. Run the built-in health check:

```bash
devboy test slack
```

The command prints the granted scopes and a `Missing scopes` line if anything on the required list is absent. Relevant for notifications:

- `chat:write` — required to post.
- `channels:read` / `groups:read` / `im:read` / `mpim:read` — required for the `get_messenger_chats` lookup in step 1.

If `Missing scopes` is non-empty, **do not call `send_message`.** Surface the missing-scope list back to the user with a clear remediation message, e.g.:

> The Slack token is missing `chat:write`. Re-issue the bot token with the scope added (`channels:read`, `groups:read`, `im:read`, `mpim:read`, `chat:write`, …) and re-run `devboy test slack` to confirm.

The exact default list lives in `default_slack_required_scopes()` — follow that source rather than hardcoding the scope list in a conversation.

### 3. Compose the message — short and structured

Keep notifications skimmable. Target format:

```
*Release v2.4.2 deployed*
- Rollback of v2.4.1 complete, staging + prod green
- Follow-up: regression test in MR !842
- Incident post-mortem: <https://wiki/.../incident-204|incident-204>
```

Conventions:

- **One-line subject** in `*bold*`.
- **2–3 body bullets** — each a single line.
- **Optional link** on the last bullet (Slack link syntax: `<url|label>`).
- Slack markup: `*bold*`, `_italic_`, inline `` `code` ``, and triple-backtick fences for short snippets. Do not paste long logs — link to them instead.
- Mentions only when the user asked for them. On Slack: `<@U0123ABCD>` for a user, `<!subteam^SXXXXXXX>` for a group, `<!here>` / `<!channel>` sparingly.

### 4. Confirm the draft with the user (if there's any ambiguity)

If the user's wording left the text or the target open, show the drafted subject + bullets + target chat name and wait for a "go". If the user was explicit ("post exactly this to #eng"), skip the confirmation.

### 5. Send

```bash
devboy tools call send_message '{
  "chat_id": "C0123456789",
  "text": "*Release v2.4.2 deployed*\n- Rollback of v2.4.1 complete, staging + prod green\n- Follow-up: regression test in MR !842\n- Incident post-mortem: <https://wiki/.../incident-204|incident-204>"
}'
```

For a threaded reply, add `thread_id` (or `reply_to_id` where the provider supports it):

```bash
devboy tools call send_message '{
  "chat_id": "C0123456789",
  "thread_id": "1713052800.001500",
  "text": "Patch merged — closing the thread."
}'
```

### 6. Report back

After `send_message` returns, tell the user where you posted (chat name + permalink if the response supplies one) and echo the final text. If the call failed, surface the provider error verbatim — don't swallow it.

## Success criteria

- The message was posted to the chat the user named, not a near-miss.
- The subject, bullets, and any link render cleanly in the target messenger (Slack markup, not raw Markdown).
- If scopes were missing, the skill failed **early** with a clear remediation rather than silently dropping the notification.

## Guardrails

- **Never post without an explicit ask.** "We could maybe tell the team" is not an ask. Draft and confirm.
- **Never post unreviewed generated content** (logs, AI-generated summaries, raw tool output) into a channel. Summarise first, show the user, then send.
- **Honour channel audience.** Ask for the target when the user says "the team" without naming a chat — there is almost always more than one plausible channel.

## Non-goals

- **No scheduling or recurring sends.** The tool bundle does not expose scheduled messages today — if the user asks for one, say so and suggest the native messenger scheduler.
- **No multi-channel fan-out.** This skill posts to one `chat_id` per invocation. For cross-posts, call it once per target and surface each result.
- **No message edits / deletes.** `send_message` is the only write tool in the messenger bundle today.
