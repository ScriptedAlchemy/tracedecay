# Branches and linked worktrees

TraceDecay keeps one registered project authority for a repository. Branches
and linked worktrees do not get separate databases or fact stores.

## What changes across a checkout

Every indexed code generation records the exact repository, worktree, ref,
commit/index/worktree snapshot, configuration, and source watermark that
produced it. The daemon publishes a generation only after validation, so reads
either select an exact complete generation or return a typed
stale/refresh-required/partial/unavailable state. They never silently fall back
to whichever branch happens to be active.

Linked worktrees share the primary checkout's registered project identity and
embedded project Grafeo store. Content-addressed extraction artifacts may be
reused when their complete identity matches, while generation and provenance
remain exact to each worktree snapshot.

## Facts and sessions

Project facts and project sessions are project-wide. A fact or observation may
record branch, ref, worktree, commit, PR, session, or agent provenance, but
deleting or renaming any of those cannot delete, move, archive, merge, or hide
the fact. Cross-branch recall filters or explains provenance; it does not route
to a branch-local store.

## Normal workflow

1. Enroll the repository once.
2. Open any registered checkout or linked worktree.
3. Let file/Git hooks submit bounded change signals to the daemon, or request an
   explicit refresh when a host cannot signal historical/missed data.
4. Inspect the returned generation/snapshot and coverage on every query whose
   freshness matters.
5. Use explicit worktree/ref/commit selectors for comparisons; never infer
   identity from a database path.

MCP, CLI, LSP, dashboard, SDK, hooks, and hosts all use the same
daemon/application route. None opens a graph or relational database directly.

## Failure states

- `refresh_required` or stale: the selected snapshot is newer than the
  published generation; request or wait for daemon refresh.
- partial/unavailable: retain returned evidence and coverage; do not treat it
  as a complete empty result.
- reset required: the persisted TraceDecay shape is incompatible with final
  V2. Recreate the profile/project store explicitly; no migration or fallback
  reader exists.
- identity mismatch: re-enroll or correct the registered repository/worktree
  identity through the daemon. Do not copy, rename, or edit database files.

Doctor is read-only and reports these states. Any authorized maintenance effect
is a separate daemon application operation with its own receipt.
