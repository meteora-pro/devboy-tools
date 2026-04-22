---
name: devboy-meeting-transcript
description: Fetch a meeting transcript — full, paginated, or filtered to a single speaker or phrase — without leaking raw PII into long-term storage.
category: meeting-notes
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "transcript of meeting"
  - "what did Alice say about"
  - "show the meeting transcript"
  - "quote from the meeting"
  - "what was said about"
tools:
  - get_meeting_transcript
  - get_meeting_notes
---

# devboy-meeting-transcript

Retrieve the speaker-attributed, timestamped transcript for a single meeting. Transcripts are routinely large (thousands of sentences for a one-hour call), so the skill is built around pulling only what the question actually needs rather than the whole thing.

## When to use

- The user asks for a transcript by meeting title, id, or relative reference ("the call yesterday", "the one with Alice on Tuesday").
- The user wants a specific quote ("what did Bob say about the migration?").
- A follow-up skill (`devboy-meeting-to-tasks`) needs the raw text to extract action items that are not already in `action_items`.

If the user does not yet know which meeting — route through `devboy-meeting-search` first.

## Procedure

### 1. Resolve the meeting id

You need a `meeting_id`. If the user already pasted one, skip this step. Otherwise confirm the id via `get_meeting_notes` (or via `devboy-meeting-search` for a keyword lookup) and pin the id before fetching:

```bash
devboy tools call get_meeting_notes '{
  "from_date": "2026-04-16T00:00:00Z",
  "to_date":   "2026-04-17T00:00:00Z",
  "limit": 10
}'
```

Confirm the title and date with the user before proceeding — pulling the wrong transcript is an irreversible PII leak into the chat.

### 2. Fetch the transcript

```bash
devboy tools call get_meeting_transcript '{"meeting_id": "<id>"}'
```

`devboy tools call` takes a tool name + a JSON argument object — it does not accept `--budget` (or any other CLI flag) for the tool itself. The format-pipeline budget is applied inside the tool runtime. The transcript output is rendered as plain text, not JSON; a long transcript may be shortened by the pipeline and picked up again via a subsequent call.

### 3. If the rendered transcript is incomplete, fetch the next chunk

If the response ends mid-thought, pull the next segment:

```bash
devboy tools call get_meeting_transcript '{"meeting_id": "<id>", "chunk": 2}'
```

`chunk` is a per-call argument understood by the transcript tool (the default is 1). The response is still text — do not expect a JSON body with fields like `total_chunks` or `chunk_number`. The tell for "more to fetch" is that the text ends mid-sentence, not a structured field.

### 4. Prefer a targeted fetch over a full dump

When the question is specific ("what did Alice say about X?"), do not scroll the whole transcript in the chat. Two patterns:

1. **Search first, transcript second.** Run `devboy-meeting-search` with the topic word, confirm this is the right meeting, then open the relevant chunk.
2. **Grep the chunk.** The transcript is rendered as text lines shaped like `[mm:ss] <speaker>: <sentence>`, so filter with `grep` / `ripgrep`:

   ```bash
   devboy tools call get_meeting_transcript '{"meeting_id": "<id>"}' \
     | grep -F '] Alice: '
   ```

### 5. Quote, do not paste

When you respond to the user, reply with the minimum quote that answers the question — typically one to three sentences plus the speaker and timestamp. Never paste the full transcript into the chat unless the user explicitly asked for it.

## Success criteria

- The user gets an answer grounded in the transcript, with speaker + timestamp, in fewer than 10 lines for a targeted question.
- For a "give me the whole transcript" request, the skill streams chunk 1, reports `total_chunks`, and waits for the user to ask for more rather than pre-fetching all chunks.
- No untrimmed transcript content is copy-pasted into files the user did not name.

## Guardrails

- **PII.** Transcripts contain names, emails, internal project names, and sometimes customer data. Do not write them to long-term storage (new docs, new issues, commit messages) unless the user explicitly asks for a specific excerpt. Summarise; do not mirror.
- **Do not translate or edit quotes.** If the user asks "what did Alice say?", answer with her words, not a paraphrase — downstream uses (legal, HR, customer conversations) rely on the exact phrasing.
- **Confirm the meeting before fetching.** A wrong `meeting_id` leaks a different meeting's contents into the chat and is not retractable.

## Non-goals

- Editing or redacting transcripts. The skill is read-only.
- Summarising a full meeting. Use the `summary` and `action_items` fields returned by `get_meeting_notes` — those are provider-generated and cheaper to fetch than the full transcript.
- Cross-meeting aggregation. One transcript at a time.
