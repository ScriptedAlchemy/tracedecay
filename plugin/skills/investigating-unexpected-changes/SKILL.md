---
name: investigating-unexpected-changes
description: 'Use when unexpected changes appear on your branch or worktree — commits you did not make, amended or force-pushed history, or a working tree that contradicts your memory — to reconstruct who did what and decide how to proceed.'
---

# Investigating unexpected changes

Parallel agents share branches and worktrees. Real sessions have hit this and
guessed: one Claude session found three of its branches amended under it and
called it "auto-merged... a repository watcher"; a Cursor session saw a test
file it "didn't write but which passes" and spent turns on blind `git log`
trying to explain it. Blind git archaeology cannot name the actor. TraceDecay
correlates every session to the branches, worktrees, and commits it touched, so
you can attribute the change instead of theorizing. Do that before you react.

## Establish what changed

Pin the objective facts first; do not act on a hunch.

1. Local ground truth: `git status`, `git log --oneline -20`, and
   `git reflog -20` (reflog survives amends and resets — it shows HEAD moving
   even when `git log` looks clean).
2. Semantic shape of the delta:
   - Uncommitted working-tree drift → `tracedecay_commit_context` (changed
     symbols, file roles, recent commit style) to see *what* moved without
     reading raw diffs.
   - Branch moved vs. where you expected → use Git's live merge-base and
     `git diff --name-only <base>...<head>` as the file boundary, then pass
     those paths to `tracedecay_diff_context`. Treat `tracedecay_branch_diff`
     as a summary only after its commits/files agree with that live boundary;
     never act on a stale graph snapshot alone.

## Attribute it

Turn "what changed" into "who changed it" with the session-git correlation
index.

1. `tracedecay_sessions_for` (`git_ref`: `branch` | `worktree` | `commit`,
   `value`, optional `since` / `until`) lists every session correlated with the
   artifact. Query the surprising commit sha first, then the branch or worktree
   if the commit is unattributed.
2. Read the `relation` on each returned session:
   - `produced` — that session *made* the commit (the actor you are looking
     for).
   - `observed` — that session only *saw* it (a bystander, useful for timeline
     but not authorship).
   - unclassified legacy rows have no relation and still match; treat them as
     candidates, not proof, and corroborate with the session's messages below.
3. Attribution is span-based, so a session that switched branches mid-run is
   credited only for the spans it actually held — a `produced` hit is
   trustworthy even across branch churn.

## Read the acting session's context

Once you have the acting session id, recover *why* it made the change.

1. `tracedecay_message_search` (`query`, plus `branch` / `commit` filters, or
   `parent_session_id`) surfaces what the actor was told to do and what it
   reported — search the commit sha or the touched file names.
2. `tracedecay_lcm_grep` with `scope: "session"` and that `session_id` returns
   bounded raw-message snippets for the one session; `scope: "all"` is the
   cross-session default.
3. Heed the `capped_sessions` disclosure: a broad (`scope: "all"`) grep can cap
   how many sessions it scans and say so. When the acting session may have been
   capped, rerun with `scope: "session"` and its id for complete results before
   concluding.

## Decide

With the actor, their intent, and the delta in hand, choose:

- Compatible and wanted → adopt it: rebase or fast-forward onto the new HEAD and
  continue.
- Conflicting or unwanted → coordinate: the `produced` session id and its goal
  tell you whom to reconcile with rather than force-pushing over live work.
- Ambiguous ownership → do not overwrite. A force-push over another agent's
  `produced` commit destroys unmerged work.

If a durable ownership convention emerges (for example "branch `codex/*` is
owned by the Codex worktree"), record it with `tracedecay_fact_store_add` so
the next session inherits it — see `tracedecay:project-memory`.

## If tools are deferred or MCP fails

- Deferred (names listed without schemas): load once with ToolSearch —
  `select:tracedecay_sessions_for,tracedecay_commit_context,tracedecay_diff_context,tracedecay_branch_diff,tracedecay_message_search,tracedecay_lcm_grep`
  (one batched call) — then call normally.
- MCP error / timeout / disconnect: same tools via shell —
  `tracedecay tool sessions_for --args '{"git_ref":"commit","value":"<sha>"}'`,
  `tracedecay tool commit_context`, `tracedecay tool diff_context`,
  `tracedecay tool branch_diff`,
  `tracedecay tool message_search`, `tracedecay tool lcm_grep` (see
  `tracedecay:using-the-cli`). Never query `.tracedecay` databases directly;
  never fall back to blind git archaeology when the correlation index can name
  the actor.

## Deliverable

Do not end without: the objective delta (which commits/files/symbols changed and
how HEAD moved), the attributed actor (session id + `relation`, or an explicit
"unattributed" when no session correlates), that session's intent from its
messages, and the chosen action (adopt / coordinate / hold). Report any
`tracedecay_metrics:` line.
