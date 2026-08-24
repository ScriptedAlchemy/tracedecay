# Comparable tools

This is a scope comparison, not a conformance matrix. Other products change
independently, so their own documentation is the authority for their current
interfaces, pricing, privacy policy, language support, and performance.

## TraceDecay's current product boundary

TraceDecay is a local-first, daemon-owned code-intelligence and agent-memory
product. Clients submit typed operations to one daemon authority; they do not
open or modify TraceDecay stores directly. Grafeo owns admitted graph relations,
traversals, and vectors. SQLite owns relational and content records, manifests,
journals, leases, receipts, and watermarks. Neither is a substitute or a shadow
copy of the other.

The supported user journey is to enroll a project, let the daemon converge its
immutable generation in the background, and read the result's identity,
provenance, coverage, and receipt. A read can report `warming`, `partial`,
`refresh_required`, `unavailable`, `denied`, or `cancelled`; none of those is an
empty successful result.

This page deliberately makes no current comparison claim for a dashboard graph
visualizer, semantic retrieval, multi-root federation, or an authorized
refactoring effect. Those capabilities must not be inferred from internal
plans, generated contracts, or a competitor's feature list.

## How to compare products responsibly

| Question | What to verify |
|---|---|
| Evidence authority | Whether the product names the project, snapshot/generation, coverage, and source of a result. |
| Data ownership | Whether a client can bypass the product's durable authority or whether writes and maintenance have an explicit owner. |
| Freshness | Whether background convergence and stale/partial states are visible instead of being hidden behind an implicit refresh. |
| Effects | Whether a change has an authorization boundary, preview where appropriate, durable receipt, and recovery semantics. |
| Privacy and network | Whether local processing, remote effects, request metadata, configuration, and offline/denied behavior are described separately. |
| Host support | Whether the claimed host journey can be installed, diagnosed, updated, and removed on the current host version. |

For TraceDecay, confirm those details against the daemon rather than prose:

```bash
tracedecay status --json
tracedecay doctor
tracedecay tool
```

`status` identifies the selected authority and generation. `doctor` is
read-only: it reports installation and authority state, but does not repair,
refresh, rewrite, or recreate a store. Use the explicit daemon operation named
by a typed diagnostic when a maintenance action is authorized.

## Choosing TraceDecay

TraceDecay is appropriate when an agent needs code and session evidence from a
single daemon authority, with exact project/worktree/ref/commit provenance and
truthful coverage. It is not a promise that a generic context prefill product,
an embedding-first search product, a visualization product, or an autonomous
refactoring system is being replaced.

See [the user guide](USER-GUIDE.md), [the branching guide](BRANCHING-USER-GUIDE.md),
and [the V2 product contract](plans/tracedecay-v2/00-plan-set-index.md) for the
supported journeys and their state semantics.
