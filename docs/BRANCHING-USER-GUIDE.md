# Multi-Branch Indexing Guide

## The problem

TraceDecay maintains a code graph in the active project store. Without multi-branch indexing, that store has one SQLite database for the project. When you switch
git branches, the files on disk change but the graph still reflects the old branch. The
MCP staleness check or daemon/server hook event path eventually catches up by re-indexing changed files, but there are two costs:

1. **Stale window.** Between the checkout and the next sync, every MCP query returns results
   from the old branch. A symbol search might surface a function that doesn't exist on the
   current branch, or miss one that was just added.

2. **Redundant re-indexing.** If you alternate between `main` and `feature-x`, every switch
   triggers a differential sync that re-parses the files that differ between the two branches.
   On large projects this adds up to minutes of wasted CPU and disk I/O per day.

Multi-branch indexing solves both problems by keeping a separate database per branch. Each
branch's graph is always accurate, switching is instant, and sync work targets only the branch
you're actually working on.

## How it works

Multi-branch is fully opt-in. Without it, tracedecay behaves exactly as before: one database,
one graph, sync re-indexes whatever is on disk.

When you opt in, tracedecay creates a `branch-meta.json` file inside the active project store that tracks
which branches have their own database. In repo-local mode, the storage layout looks like this:

```
.tracedecay/
  tracedecay.db             # default branch (main/master)
  branch-meta.json          # branch tracking metadata
  branches/
    feature_foo.db          # one DB per tracked branch
    release_3_4.db
```

Projects indexed before the rebrand may still use a legacy `.tracedecay/` directory
with the same layout; it is honored as a fallback.

Profile-backed projects keep the same logical layout in their profile shard. The repository may contain only an enrollment marker, while `branch-meta.json`, `tracedecay.db`, and `branches/*.db` live under the resolved store root.

Creating a new branch database is cheap. TraceDecay copies the nearest ancestor's database
(usually `main`) and then runs an incremental sync that only re-parses files whose content
hash differs from what's in the copy. If your branch touches 20 files out of 2,000, only
those 20 get re-indexed.

## Getting started

### Track your first branch

From a feature branch:

```
tracedecay branch add
```

This detects the current branch name, copies the nearest tracked ancestor's database,
and syncs the diff. If no branch metadata exists yet, it bootstraps it automatically.

You can also track a branch by name without checking it out:

```
tracedecay branch add feature/new-parser
```

### See what's tracked

```
tracedecay branch list
```

Output:

```
Default branch: main

  main * — 206.3 MB, synced 5m ago
  feature/foo — 207.1 MB (from main), synced 2h 10m ago
  release/3.4 — 205.8 MB (from main), synced 1d ago
```

The `*` marks the currently checked-out branch. Each entry shows the database size, which
branch it was copied from, and when it was last synced.

### Remove a tracked branch

```
tracedecay branch remove feature/foo
```

This deletes the branch's database and removes its entry from `branch-meta.json`. The
default branch cannot be removed.

### Clean up stale branches

After you merge and delete branches in git, their databases linger. To remove databases
for branches that no longer exist:

```
tracedecay branch gc
```

This checks each tracked branch against `.git/refs/heads/` and `packed-refs`, and deletes
databases for branches that are gone.

## How branch-aware sync handles branches

MCP staleness checks and daemon/server hook events behave differently depending on whether multi-branch is active:

**Without multi-branch (default):** Sync updates the single `tracedecay.db`. Switching branches triggers a sync of all changed files.

**With multi-branch:** Before each sync, TraceDecay checks the current branch. If that branch
is tracked, it syncs that branch's database. If it's not tracked, it syncs the default
branch's database. After syncing, it updates the `last_synced_at` timestamp in the metadata.

You don't need to restart the MCP server after adding a branch. Sync picks up metadata
changes on the next sync cycle.

## How the MCP server selects a database

When the MCP server starts (via `tracedecay serve`), it reads `.git/HEAD`
to determine the current branch and opens the corresponding database.

If the current branch is tracked, queries run against its own database with full accuracy.

If the current branch is not tracked, the server falls back to the nearest tracked ancestor
(determined by `git merge-base`). Every tool response is prepended with a warning:

```
WARNING: branch 'experiment-x' is not tracked — serving from 'main'.
Run `tracedecay branch add experiment-x` to track it.
```

This means queries still work, but results may be stale for files that differ between the
branches.

## Cross-branch queries

Two MCP tools let you query across branches without switching your checkout:

### Search in another branch

`tracedecay_branch_search` searches for symbols in a different branch's graph:

```json
{
  "branch": "main",
  "query": "parse_config",
  "limit": 5
}
```

This opens `main`'s database, runs the search, and returns results tagged with the branch
name. Useful for checking whether a symbol exists on `main` before you try to use it.

### Compare branches

`tracedecay_branch_diff` compares the code graphs of two branches:

```json
{
  "base": "main",
  "head": "feature/foo"
}
```

Returns three lists:

- **added**: symbols present in `head` but not in `base`
- **removed**: symbols present in `base` but not in `head`
- **changed**: symbols present in both but with different signatures

You can filter by file path or symbol kind:

```json
{
  "base": "main",
  "head": "feature/foo",
  "file": "src/parser.rs",
  "kind": "function"
}
```

Both `base` and `head` default to sensible values: `base` defaults to the project's default
branch, `head` defaults to the current branch. So a bare `tracedecay_branch_diff {}` with no
arguments compares the current branch against `main`.

## Disk usage

Each branch database is a full copy of the graph (not a delta). For a project with a 200 MB
index, each tracked branch adds roughly 200 MB. Plan accordingly:

| Tracked branches | Approximate disk usage |
|------------------|-----------------------|
| 1 (default only) | 200 MB |
| 3 | 600 MB |
| 5 | 1 GB |
| 10 | 2 GB |

Cleanup is manual. TraceDecay never deletes branch databases automatically. Use
`tracedecay branch gc` to clean up after merges, or `tracedecay branch remove` to
delete specific branches.

## Backward compatibility

Multi-branch is fully backward compatible:

- If `branch-meta.json` doesn't exist, tracedecay operates in single-database mode exactly
  as before. No behavior changes, no new files, no extra disk usage.
- Running `tracedecay branch add` for the first time creates `branch-meta.json` and the
  `branches/` directory. The existing `tracedecay.db` becomes the default branch's database
  with zero migration.
- `tracedecay sync` and `tracedecay sync --force` continue to work. With multi-branch active,
  they sync the current branch's database.

## Auto-tracking open PR branches (daemon)

The daemon can automatically track every open pull request on a repo's GitHub
`origin` remote, so `tracedecay_branch_search` / `tracedecay_branch_diff` and
graph queries work against every open PR without running `tracedecay branch add`
by hand. When a PR merges or closes, its branch is untracked and its per-branch
store is cleaned up. The feature is **off by default**.

### Enabling it

Per project, via the CLI:

```
tracedecay branch autotrack enable                 # default 300s poll cadence
tracedecay branch autotrack enable --poll-secs 120 # custom cadence (min 60)
tracedecay branch autotrack disable
tracedecay branch autotrack status                 # show state + tracked PR branches
```

or in the dashboard **Settings → Indexing → Auto-track open PR branches**, or by
editing the `[sync]` table in the project `config.json`:

```json
{
  "sync": {
    "auto_track_pr_branches": true,
    "auto_track_pr_poll_secs": 300
  }
}
```

Both keys default off/`300` and are back-compatible: a `config.json` predating
them keeps the feature disabled. Environment overrides
`TRACEDECAY_SYNC_AUTO_TRACK_PR_BRANCHES` and
`TRACEDECAY_SYNC_AUTO_TRACK_PR_POLL_SECS` apply on top. The poll interval is
clamped up to a 60-second floor. Changes take effect after a daemon restart
(`tracedecay daemon restart`).

### How it works

Each poll, the daemon discovers open PR heads — via `gh pr list` when `gh` is on
`PATH` and `origin` is GitHub, otherwise via `git ls-remote origin
'refs/pull/*/head'`. For each new same-repo PR it fetches `refs/pull/<N>/head`,
checks it out into a linked worktree under the store's `pr-worktrees/` directory
on a synthetic branch `pr/<N>`, and tracks that worktree exactly like any other
branch (so the PR's own content is indexed, not your current checkout). Tracked
PR branches appear in `tracedecay branch list` as `pr/<N>`. To keep a repo with
many open PRs from stampeding, at most 10 new PR branches are tracked per poll
cycle; the rest ramp up on subsequent cycles.

The daemon logs structured events: `event=pr_autotrack action=tracked|untracked|skipped
branch=pr/<N> pr=<N>` per change and an `action=poll` summary each cycle.

### Scope: same-repo PRs only

Only PRs whose head branch lives on the repo itself are tracked. Fork PRs (head
on a different repository) are **skipped** with a logged reason
(`action=skipped reason=fork`). Discovery treats a PR as a fork when its head
SHA matches no `refs/heads/*` ref on `origin` (or, via `gh`, when
`isCrossRepository` is true).

## FAQ

**Does rebasing a branch break its database?**
No. TraceDecay syncs by comparing file content hashes on disk against what's stored in the
database. It doesn't track git commit history. After a rebase, the next sync re-indexes
whatever files actually changed, regardless of how the history was rewritten.

**Can I query a branch I haven't checked out?**
Yes, using `tracedecay_branch_search` and `tracedecay_branch_diff`. These open the target
branch's database directly without requiring a checkout.

**What happens on detached HEAD?**
The MCP server falls back to the default branch's database with a warning. Sync updates
the default branch's database.

**Does this work with worktrees?**
Each worktree has its own `.git/HEAD` pointing to a different branch. As long as each worktree
has been indexed (has a `.tracedecay/` directory), multi-branch works independently in each one.

**Can I track branches that only exist on the remote?**
No. The branch must have a local ref in `.git/refs/heads/`. Run `git checkout` or
`git switch` to create a local tracking branch first.

**Something went wrong — a branch shows stale results or a missing database.**
See [MULTI-BRANCH-RECOVERY.md](MULTI-BRANCH-RECOVERY.md) for a step-by-step
diagnosis-and-recovery runbook (inspect active/serving branch state, rebuild or
copy a branch DB safely, reset serving-branch fallback).
