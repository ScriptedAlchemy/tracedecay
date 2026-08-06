# Why TraceDecay?

TraceDecay is for agents that need attributable code and session evidence from
one local daemon authority. It registers a project, captures exact code
generations, and returns typed results with provenance and coverage instead of
requiring clients to inspect or synchronize a database themselves.

The persisted split is intentional: embedded Grafeo owns the admitted
graph/vector query projection, while canonical events, facts, content,
reconstruction manifests, journals, leases, receipts, and verified watermarks
remain in their domain/relational stores. A Grafeo projection is served only
after recovered-state verification. Clients never choose a store by path or
open either store directly.

## What users can rely on

- Routine code changes are submitted as bounded host/MCP/LSP/workspace hints.
  The daemon performs convergence; a normal read does not run a hidden sync.
- Results identify their project, repository/worktree/ref/commit/snapshot and
  generation context. A result that is `warming`, `partial`,
  `refresh_required`, `unavailable`, `denied`, or `cancelled` remains visibly
  in that state.
- Project facts, sessions, and lossless context belong to the registered
  project. Branch and worktree labels are provenance, not separate fact stores.
- Diagnostics are read-only. A refresh, retention change, repair, recreation,
  host change, or repository effect is a separate daemon operation with its
  own authorization and durable receipt where applicable.

## What this page does not promise

TraceDecay does not use this comparison page to promise a dashboard graph
visualizer, semantic retrieval, multi-root federation, or an authorized
refactoring effect. Do not infer those capabilities from roadmap documents,
generated API contracts, or a comparison with another product.

Other tools may be a better fit when their current, supported journey is the
one you need: passive context prefill, model-backed search, visual exploration,
or a particular automated code transformation. Confirm those products' current
behavior, data handling, and host support in their own documentation.

## Before installing or troubleshooting

Start with the supported local checks:

```bash
tracedecay init
tracedecay install
tracedecay status --json
tracedecay doctor
```

`doctor` reports state without changing it. When it reports a typed condition,
use the documented daemon operation for that condition rather than copying,
editing, or querying a TraceDecay store directly. See [Comparable tools](COMPARABLE-TOOLS.md)
for the comparison boundary and [Privacy and Network](USER-GUIDE.md#privacy-and-network)
for local and remote-effect details.
