---
name: meeting-search
description: Find meetings by keyword, participant, host, or date range and surface a short, rankable hit list.
category: meeting-notes
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "find meeting about"
  - "when did we discuss"
  - "list meetings last week"
  - "search meetings"
  - "find calls with"
tools:
  - search_meeting_notes
  - get_meeting_notes
---

# devboy-meeting-search

Answer "which meeting did we talk about X?" without making the user scroll through a calendar. The skill narrows a corpus of meeting notes down to a short hit list the user can act on — drill-down into a single meeting's metadata is a follow-up step, and pulling the full transcript is the job of `devboy-meeting-transcript`.

## When to use

- The user names a topic, keyword, or phrase ("the migration call", "anything about SSO") and wants to locate the meeting.
- The user wants a window of recent meetings ("what did we have this week?", "list calls with Alice last month").
- Before running `devboy-meeting-to-tasks` or `devboy-meeting-transcript`, confirm the target meeting id.

If no search term is given — just a window or a participant — skip the keyword search and go straight to `get_meeting_notes` with the filter.

## Procedure

### 1. Keyword search

Use `search_meeting_notes` when the user supplied a topic. All filters (date range, participants, host) combine with the keyword on the provider side — there is no client-side filter.

```bash
devboy tools call search_meeting_notes '{
  "query": "migration",
  "from_date": "2026-03-01T00:00:00Z",
  "to_date":   "2026-04-17T00:00:00Z",
  "limit": 20
}'
```

Dates are ISO 8601. The tool caps `limit` at 50 per call; paginate with `offset` if you need a deeper sweep.

### 2. Filter-only listing (no keyword)

```bash
devboy tools call get_meeting_notes '{
  "from_date": "2026-04-10T00:00:00Z",
  "participants": ["alice@example.com"],
  "limit": 20
}'
```

Use this for "last N days" or "calls with Alice" — no keyword means do not invent one.

### 3. Rank the hits

The provider returns meetings newest-first but is not topic-ranked. When several hits look plausible, resolve ties by:

1. **Recency** — the user usually wants the most recent occurrence.
2. **Participant overlap** — prefer meetings whose attendees match the user's context (the person they are collaborating with, the team they asked about).
3. **Keyword density** — if the provider surfaces `summary`, `keywords`, or `topics_discussed`, a meeting that hits multiple of them is a stronger match than one that mentions the term in passing.

Present at most 5–10 candidates. If more exist, say so and offer to narrow by date or participant rather than dumping them all.

### 4. Drill down to one meeting

Once the user picks a hit, fetch its metadata for a short card — title, date, duration, participants, action-items count, a one-line summary:

```bash
devboy tools call get_meeting_notes '{
  "from_date": "<date of the hit>",
  "to_date":   "<+1 day>",
  "limit": 5
}'
```

(There is no single-meeting "get by id" helper — narrow the date window and match on `id`.)

## Success criteria

- The user can name the meeting they meant without reading a transcript.
- For every hit the agent shows, at minimum: title, date, 1-line summary or top keyword.
- If zero hits come back, the skill suggests a broader filter (wider date range, different keyword) rather than inventing a meeting.

## Guardrails

- **Do not dump transcripts here.** A search result is metadata + a one-line summary. Full transcript retrieval lives in `devboy-meeting-transcript`.
- **Do not paraphrase the summary.** Quote the provider's summary verbatim — it is short and PII-sensitive; rewording risks distortion.
- **Participants are emails.** When the user says "calls with Alice", ask for the email before filtering — display names from the provider are not guaranteed unique.

## Non-goals

- Ranking by semantic similarity beyond what the provider returns. The tool is a keyword search, not a vector search.
- Cross-provider search. Only the meeting-notes provider that is configured (e.g. Fireflies) is queried.
