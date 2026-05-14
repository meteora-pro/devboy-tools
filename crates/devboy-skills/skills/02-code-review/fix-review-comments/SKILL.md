---
name: fix-review-comments
description: Apply reviewer feedback on an MR — fix the code, run local checks, commit, push, and reply to each discussion.
category: code-review
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "fix review comments"
  - "address MR review"
  - "apply reviewer feedback"
  - "address PR feedback"
tools:
  - get_merge_request_discussions
  - get_merge_request_diffs
  - create_merge_request_comment
---

# fix-review-comments

Address a reviewer's feedback on one MR / PR. For every unresolved discussion: decide whether to accept, push back, or clarify; if accepting, make the change locally and verify it; then reply to the thread with a short acknowledgement.

## When to use

- A reviewer left comments on your MR and you are ready to address them.
- The user says "fix review comments", "apply reviewer feedback", or similar with an MR key.
- You are the author of the MR. For reviewing **someone else's** MR use `review-mr`.

## Procedure

### 1. Pull every open discussion

```bash
devboy tools call get_merge_request_discussions '{"key": "mr#374", "limit": 100}'
```

The tool returns `Discussion { id, resolved, comments, position }`. `comments` is the thread's list of messages; `position` carries the file path + line number for inline discussions. Reply later using the discussion's `id` (for GitLab) or the numeric id of one of `comments[*]` (for GitHub — see the reply section below).

**Do not rely on `resolved` alone to pick threads that need a reply.** On GitHub the provider has no reliable resolved-state signal in the REST data it reads, so `resolved` is always `false` — treating "unresolved" as "needs reply" will pick every thread and produce infinite reply loops on re-runs. Use a deterministic filter instead:

- threads where the latest comment is not authored by the MR author, **or**
- threads where the MR author has not replied since the reviewer's last comment.

On GitLab the `resolved` field is populated correctly and you may lean on it as a hint, but keep the author-based check as a fallback — it works across providers.

### 2. Pull the current diff for context

```bash
devboy tools call get_merge_request_diffs '{"key": "mr#374"}'
```

You need the diff to reason about where each comment applies — reviewers sometimes comment on context lines, not changed lines.

### 3. Classify each discussion

Walk the discussions in the order the reviewer posted them. For each one, pick exactly one disposition:

- **Accept** — the reviewer is right; apply the fix locally.
- **Push back** — you disagree or the suggestion is out of scope; reply with a one- or two-sentence reason and keep the scope to this thread.
- **Clarify** — the comment is ambiguous; reply asking for the specific piece of information you need.

Do not batch dispositions. Decide per discussion.

### 4. Apply fixes (for "accept")

Edit the files locally. Keep the change minimal — a review fix is a fix, not a refactor (see guardrails below). After **each** logical fix, run the local checks appropriate for the stack touched. For `devboy-tools`:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test -p <the-crate-you-touched>
```

If any of those fails, fix the root cause before moving on — do not commit a half-green state.

### 5. Commit and push

Group related fixes into one commit where it reads naturally. Commit messages reference the scope, not the reviewer:

```bash
git add -- <paths>
git commit -m "fix(cli): surface a real error when the env var is unset (DEV-XXX)"
git push
```

Note the commit SHA of each fix — you will reference it in the reply. For fixes that span multiple discussions, one commit covering several threads is fine as long as the body of the commit lists them.

### 6. Reply to each discussion

Use `create_merge_request_comment` in **reply mode**. The right reply id depends on the provider:

- **GitLab** — reply with the discussion's `id` as `discussion_id`. GitLab threads are first-class objects and the `Discussion.id` is what you pass back.
- **GitHub** — GitHub's REST reply goes through `in_reply_to` on a **review comment id**, which is numeric. The provider packs that into a `Discussion.comments[*].id`; pass the id of the comment you are replying to (typically the last one in the thread) as `discussion_id`.

If you get a 404 on reply, the value was the wrong one for the provider — fall back to a top-level comment rather than looping.

Keep the reply one or two sentences:

- **Accepted**: `fixed in <sha>` or `fixed in <sha> — <one-line what changed>`.
- **Pushed back**: `keeping as-is because <specific technical reason>`.
- **Clarified**: `could you clarify <specific point>? <short reason you're asking>`.

```bash
# GitLab example
devboy tools call create_merge_request_comment '{
  "key": "mr#374",
  "discussion_id": "abc123",
  "body": "fixed in 9f2a1e4 — switched to `Error::Config` so the CLI prints a real message."
}'
```

Do not pass `file_path` / `line` when replying — the thread already owns a position.

### 7. Verify

```bash
devboy tools call get_merge_request_discussions '{"key": "mr#374", "limit": 100}'
```

Every thread you handled should now have your reply as the latest note. If a thread is missing your reply, the call either targeted the wrong `discussion_id` or the provider rejected the body — re-try.

## Success criteria

- Every previously-unresolved discussion has a reply from you.
- Every "accept" reply names the SHA that contains the fix.
- Local `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and the relevant `cargo test` pass.
- The branch is pushed; the MR shows the new commits.
- No discussion is left silent because you "could not figure it out" — clarify instead.

## Guardrails

- **Never mark a discussion resolved for a reviewer who did not ask you to.** Some hosts expose a "resolve" action; do not call it. Resolution is the reviewer's prerogative on their next pass. Your job ends at replying.
- **Keep the push-back scoped to the thread it belongs to.** If the reviewer is wrong across several threads, reply per thread — do not escalate in a single omnibus comment.
- **Do not amend past commits from the branch** unless the reviewer explicitly asked for a squash. New commits are easier to review.
- **Do not change the MR target branch, title, or description** as part of a "fix comments" pass unless the reviewer asked for exactly that.
- **Do not introduce unrelated cleanup.** A review fix touches the lines under discussion and whatever is strictly required to make them work. Larger refactors get their own MR.

## Non-goals

- Approving or closing the MR. Resolution and merge remain with the reviewer / author's separate actions.
- Running CI for the reviewer. Pushing commits is enough — the pipeline will trigger on its own.
