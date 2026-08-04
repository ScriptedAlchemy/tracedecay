# Repository snapshot and worktree design

## Authority

One daemon-owned project shard serves every checkout and linked worktree of a
registered repository.

- Embedded Grafeo is the durable project graph/vector authority.
- SQLite retains relational/content records, manifests, journals, leases,
  receipts, and publication fencing.
- Holographic memory retains canonical project-wide fact content and
  algorithm-intrinsic state. Grafeo may store typed fact-ID relations and
  vector indexes, never duplicate fact payloads.
- Branch/ref/worktree/commit identities are provenance and selectors, not
  storage owners.

There is no branch database, graph clone, branch fact shard, default-branch
fallback, archive merge, or branch migration.

## Generation model

Capture resolves an exact repository/worktree snapshot. Extraction produces
typed content-addressed artifacts. Publication writes an immutable
generation-scoped graph projection and advances its canonical watermark only
after validation. Multiple generations can coexist inside one project graph
while active reads remain pinned to an exact generation.

A linked worktree can reuse an extraction artifact only when content,
language/parser, privacy, configuration, and model identities all match. Its
worktree/ref/snapshot provenance is still distinct.

## Routing

The daemon registry keys the exact registered project/profile identity and
canonical store location. MCP, CLI, HTTP/dashboard, LSP, SDK, hooks, workers,
automation, and hosts request typed application operations. They never choose a
database from CWD, branch name, environment override, or an active graph.

Cross-project or multi-root operations freeze every selected project identity,
generation, authorization epoch, and continuation. A missing registry or
unavailable graph is a typed state, not a fallback to another project.

## Lifecycle

Change signals are content-free and bounded. The daemon coalesces them,
captures the current worktree snapshot, publishes validated generations, and
retains prior generations while live receipts/references require them.
Worktree or ref deletion can make a derived generation collectable, but it
cannot affect project-wide facts or other live snapshots.

Fresh V2 stores are born at the final shape. Incompatible old TraceDecay stores
return `ResetRequired`; historical host transcripts and repository observations
may then be ingested through ordinary V2 capture.
