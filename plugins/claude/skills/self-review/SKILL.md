---
name: self-review
description: Dry-run a review pass over your own MR before asking a human reviewer — local checklist, no inline posting.
category: code-review
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "self review my MR"
  - "self review my PR"
  - "check my own PR"
  - "dry-run review before requesting"
tools:
  - get_merge_request_diffs
  - get_merge_request_discussions
---

# devboy-self-review

Run the `devboy-review-mr` checklist over your **own** MR before handing it to a reviewer. Findings stay local — the output is a plain-text report to the user, not comments on the MR. If anything serious turns up, you fix it and amend the branch first.

## When to use

- You are about to mark an MR as "ready for review" and want a sanity pass.
- A previous review round finished; you want to confirm nothing new slipped in while addressing the comments.
- The user says "self-review my PR" or similar.

For reviewing **someone else's** MR, use `devboy-review-mr` — it posts inline comments and a summary. Self-review deliberately does not.

## Procedure

### 1. Resolve the MR key

`mr#<n>` or `pr#<n>`. If the user did not give one, ask — do not guess from branch state.

### 2. Pull the diff

```bash
devboy tools call get_merge_request_diffs '{"key": "mr#374"}'
```

Read every changed file end-to-end. This is the only piece of ground truth you need — metadata (title, labels, target branch) is out of scope for self-review.

### 3. Pull existing discussions

```bash
devboy tools call get_merge_request_discussions '{"key": "mr#374", "limit": 100}'
```

Flag any thread that is clearly awaiting your response — for example, the latest comment was not authored by you, or it contains an explicit request for changes / clarification / follow-up.

If the provider exposes a reliable `resolved` field you may use it as a hint (GitLab does), but do not treat `unresolved` on its own as a universal blocking signal: on GitHub the provider has no reliable resolved-state signal in the REST data and `resolved` is always `false`, so a naive check would tag every thread and make self-review impossible on GitHub-hosted PRs.

Self-review is incomplete while a reviewer is still waiting on you — either reply to the thread (see `devboy-fix-review-comments`) or, if you are pushing back, have the reasoning ready so the reviewer is not left waiting.

### 4. Walk the checklist

Same list as `devboy-review-mr`, applied to your own code. For each item, record one of three outcomes: `ok`, `minor` (note it for the reviewer), `fix-before-review` (you are going to change the code right now).

- **Type safety.** Are new public APIs fully typed? Are `unwrap()` / `expect()` calls justified? Option / Result combinators rather than unwrapped access?
- **Error handling.** Do new error paths surface a useful message? Errors propagated rather than swallowed? New variants added to the right enum?
- **Tests.** Is every new behaviour covered? Edge cases (empty input, provider-unsupported, parse failure)? Assertions meaningful, not just "did not panic"?
- **Docs.** Public items documented? README / `docs/` change if the public surface changes?
- **i18n / user-facing copy.** Any `SKILL.md` body, CLI output, error message — English only?
- **Cross-platform.** No POSIX-only paths, no `bash`-only scripting, no Unix-only tools?

### 5. If any item is `fix-before-review` — fix it

Make the change, then re-run the local checks appropriate for the stack touched. For `devboy-tools`:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test -p <the-crate-you-touched>
```

Amend the branch — a new commit is fine, a squash is fine, whatever matches the project's convention. Push. Then **go back to step 2** and re-read the diff. Self-review is iterative; do not short-circuit after a fix.

### 6. Produce the report

When the list has no `fix-before-review` items left, emit a compact text report to the user. Example shape:

```
Self-review — mr#374

Type safety:         ok
Error handling:      minor — new variant `Error::ProviderStale` is not in the README table yet
Tests:               ok
Docs:                minor — new --remote-config-url flag missing from README cheatsheet
i18n:                ok
Cross-platform:      ok

Open discussions:    0
Recommendation:      ready for review — two minor notes to call out in the MR description
```

The report goes to the user in the chat. **Do not** post it as an MR comment. Any item flagged `minor` is something you mention in the MR description or a cover letter to the reviewer, not a self-posted review.

## Success criteria

- The report covers every checklist item with one of `ok` / `minor` / `fix-before-review`.
- No `fix-before-review` item is left unfixed by the time the report is written.
- Open discussions from previous rounds are counted and acknowledged.
- Nothing was posted to the MR as part of this skill.

## Guardrails

- **Never post inline comments on your own MR.** Self-posted comments clutter the reviewer's view and muddy the signal about what a fresh pair of eyes actually flagged.
- **Do not mark anything as resolved.** Resolution belongs to whichever reviewer opened the thread.
- **Do not approve, merge, or change MR state.** This skill is strictly a dry-run.
- **Do not expand scope during the fix pass.** A self-review fix is still a fix — refactors belong in a separate MR.

## Non-goals

- Replacing a human reviewer. Self-review catches the obvious; the human still needs to sign off.
- Running the full CI pipeline locally. Local checks (`cargo fmt`, `cargo clippy`, targeted `cargo test`) are enough for a self-review pass — CI is what the reviewer sees.
