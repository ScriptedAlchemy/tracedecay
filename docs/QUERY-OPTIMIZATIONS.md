# Graph query behavior

This note supersedes the retired SQL N+1 inventory. It is not an SQL tuning
guide: V2 does not expose a SQLite graph-query path for operators or clients.

The daemon/application authority selects and serves graph reads. Embedded
Grafeo owns admitted graph relations, traversals, and vectors; SQLite owns
relational and content records, manifests, journals, leases, receipts, and
watermarks. A client must not open either store, construct a parallel query, or
infer graph state from database files.

## Serving behavior

- A code read is bound to the selected project, repository, checkout, worktree,
  ref, commit/tree, snapshot, configuration, source watermark, and generation.
- The daemon performs bounded convergence after it receives a host, MCP, LSP,
  or workspace hint. Ordinary exact, lexical, graph, and other available reads
  do not block on historical convergence or force a full synchronization.
- A newer generation in progress is visible through typed coverage such as
  `warming`, `refresh_required`, `partial`, or `unavailable`; it is never
  represented as an empty successful graph.
- Diagnostics report the selected authority and coverage. An explicit refresh
  or maintenance action is separate from a read and follows its own
  authorization and receipt path.

The [V2 product contract](plans/tracedecay-v2/00-plan-set-index.md) is the
authority for these boundaries. [Index design](INDEX-DESIGN.md) describes the
generation and projection model; use `tracedecay status --json` and
`tracedecay doctor` to inspect a running installation.
