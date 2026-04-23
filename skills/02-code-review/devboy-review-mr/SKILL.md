---
name: devboy-review-mr
description: Strict but calibrated code review of a single MR or PR — checklist, inline comments, one overall summary.
category: code-review
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "review this MR"
  - "review this PR"
  - "do a code review"
  - "check this PR"
tools:
  - get_merge_requests
  - get_merge_request_diffs
  - get_merge_request_discussions
  - create_merge_request_comment
---

# devboy-review-mr

Perform a strict, calibrated code review of a single merge request / pull request. Output is one summary comment plus a handful of targeted inline comments — each tagged with `[nit]`, `[suggestion]`, or `[issue]` so the author can triage quickly.

## When to use

- The user asks "review this MR / PR" and gives an identifier (e.g. `mr#374`, `pr#128`) or a URL.
- The user pastes an MR URL without further instructions — default to this skill.
- You have been asked to gate-keep a branch before merge.

For reviewing your **own** MR before asking a human reviewer, use `devboy-self-review` — it keeps findings private instead of posting them.

## Procedure

### 1. Resolve the MR key

The tools take a key in the form `mr#<n>` (GitLab) or `pr#<n>` (GitHub). If the user gave a URL, extract the numeric id and prefix accordingly. If the user only said "this MR", ask for the key — do not guess.

### 2. Pull the MR metadata

```bash
devboy tools call get_merge_requests '{"state": "open", "limit": 50}'
```

Use this to confirm the MR exists and to pick up title, author, target branch, and labels. If only one MR is in question, you can skip this and go straight to diffs.

### 3. Pull the diff

```bash
devboy tools call get_merge_request_diffs '{"key": "mr#374"}'
```

Record every changed file and the line ranges that moved. Diffs are the primary review surface — read them before anything else.

### 4. Pull existing discussions

```bash
devboy tools call get_merge_request_discussions '{"key": "mr#374", "limit": 100}'
```

Skim every thread. Do not duplicate a comment that a previous reviewer already left — if the same concern is still unresolved, reference the existing thread in your summary instead of opening a new inline.

### 5. Walk the checklist

For every changed file, ask the following questions in order. Stop early on a file once you find one serious finding — the goal is signal, not exhaustive noise.

- **Type safety.** Are new public APIs fully typed? Are `unwrap()` / `expect()` calls justified by a local invariant, or are they latent panics? Are `Option` / `Result` chained with combinators rather than unwrapped?
- **Error handling.** Do new error paths surface a useful message the caller can act on? Are errors propagated with `?` rather than swallowed with `let _ = …` or `.ok()`? Are new error variants added to the relevant enum?
- **Tests.** Is every new behaviour covered by a unit test or an integration test? Are edge cases (empty input, provider-unsupported, parse failure) exercised? Do the new tests actually assert on meaningful output, not just "did not panic"?
- **Docs.** Are new public items documented with a short rustdoc / JSDoc block? Does the `README` or relevant `docs/` page change when the public surface changes?
- **i18n / user-facing copy.** If the diff touches a `SKILL.md` body, a CLI message, or any user-facing string, is it English? Mixed-language strings do not ship.
- **Cross-platform.** Does anything assume POSIX paths (hard-coded `/`), the presence of `bash`, or Unix-only tools? `std::path::Path` and portable commands are required.

### 6. Draft the inline comments

For each finding that deserves inline attention, prepare a short comment. Keep each one under ~150 words and start with a severity tag:

- `[nit]` — cosmetic, take-it-or-leave-it (whitespace, naming, tiny refactor).
- `[suggestion]` — real improvement that is not strictly blocking.
- `[issue]` — blocking; the reviewer wants this fixed before merge.

Long explanations, context, or cross-file reasoning belong in the summary comment, not inline.

### 7. Post the inline comments

One tool call per inline comment. File path and line must match the diff exactly.

```bash
devboy tools call create_merge_request_comment '{
  "key": "mr#374",
  "file_path": "crates/devboy-core/src/provider.rs",
  "line": 142,
  "line_type": "new",
  "body": "[issue] `unwrap()` on the env-var lookup will panic when the variable is unset. Return `Err(Error::Config(...))` instead so the caller can surface a useful message."
}'
```

`line_type` is an `old` / `new` selector for which side of the diff the `line` number refers to: `"new"` for added or unchanged (context) lines, `"old"` only for deleted lines. Using `"old"` on a context line can place the comment on the wrong side or fail outright. Default to `"new"`.

Do not pass `commit_sha` unless you already have a concrete SHA from outside this skill's tool set. `get_merge_request_diffs` returns `FileDiff` without a head SHA, so there is no head commit to lift from the tool output. On GitHub the provider fills in the PR head SHA automatically when `commit_sha` is omitted; on GitLab it is not required for line-scoped comments.

### 8. Post the summary comment

One final, top-level comment. Structure it so the author can skim in ten seconds:

```
### Review summary

- Strengths: <one or two short bullets>
- Blocking issues: <count>  (see inline [issue] tags)
- Suggestions: <count>      (see inline [suggestion] tags)
- Nits: <count>             (see inline [nit] tags)

Overall: <approve / needs changes / comment>, because <one sentence>.
```

```bash
devboy tools call create_merge_request_comment '{
  "key": "mr#374",
  "body": "### Review summary\n\n- Strengths: …\n- Blocking issues: 1 (see inline [issue])\n- Suggestions: 2\n- Nits: 0\n\nOverall: needs changes, because the new provider path panics on missing env."
}'
```

Do not pass `file_path` / `line` here — a top-level comment has no position.

## Success criteria

- Every `[issue]` you raise points at a line the author can act on, with a concrete fix path.
- The summary is a single comment, not one per finding.
- No duplicate comments — you read prior discussions before posting.
- Inline comments are short; long prose lives in the summary.
- No language other than English in any posted body.

## Guardrails

- **Do not approve or merge.** This skill posts comments only. Approval is a human action.
- **Do not resolve other reviewers' threads.** Reply if you have an opinion, but leave resolution to the person who opened the thread.
- **Do not review your own MR with this skill.** Use `devboy-self-review` instead — self-comments clutter the reviewer's view.

## Non-goals

- Running the tests yourself. The review is based on the diff and whatever CI signal is visible; actually executing the code is `devboy-self-review`'s job for the author, not the reviewer's.
- Refactoring suggestions that are out of scope for the MR. Flag them as `[suggestion]` and let the author decide.
