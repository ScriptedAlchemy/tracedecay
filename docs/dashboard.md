# TraceDecay dashboard

The dashboard is the embedded operator and investigation surface for the same
final-V2 application operations used by MCP, CLI, HTTP, LSP, SDKs, hooks, and
host integrations. It is not a second backend and never opens a TraceDecay
database.

## Start and connect

Run:

```bash
tracedecay dashboard
```

The CLI asks the daemon to start or expose the embedded dashboard and prints
the local URL. The browser connects to the daemon API with the selected
project/profile identity. A browser, plugin, or host must never receive a
SQLite path, Grafeo path, profile-root path, encryption key, or direct store
credential.

The dashboard assets are built from `dashboard/` and embedded in the Rust
binary. `dashboard/app-dist/` is generated output and is not committed.

## One application boundary

Dashboard requests use the generated wire contracts derived from canonical
Rust schemas. Hand-written TypeScript DTOs, plugin-private JSON protocols,
legacy Hermes routes, raw SQL endpoints, filesystem probes, and direct MCP
handler calls are not supported authorities.

Every request:

1. resolves profile, project, worktree snapshot, and authorization through the
   daemon;
2. invokes one transport-neutral application operation;
3. returns its typed evidence, freshness, coverage, cursor, receipt, or
   problem state; and
4. renders that result without inferring hidden success.

The dashboard does not sync, index, repair, compact, migrate, or retry merely
because a read view opened. Explicit effectful controls are separate
receipt-bearing application operations. A timed-out effect is reconciled by
receipt; it is never blindly retried.

## Workspaces

The final product exposes these integrated workspaces:

- **Brain** — project-wide holographic facts, provenance, trust, retention,
  contradictions, and graph references. Facts are never stored or sharded by
  branch. Branch, ref, worktree, commit, pull request, session, and agent may be
  recorded as provenance.
- **Explorer** — exact symbols, source, lexical search, graph navigation,
  hierarchy, callers/callees, impact, dependencies, tests, diagnostics, and
  generation freshness.
- **Loom** — bounded context packages assembled from authorized source,
  graph, session, fact, task, and delivery evidence.
- **Sessions** — live and historical host sessions, lossless LCM summary
  lineage, source pagination, temporal evolution, and content hydration from
  each message's owning store.
- **Agents** — host installation health, capabilities, sessions, handoffs,
  nested work, and bounded runtime state.
- **Delivery** — worktree and commit identity, diffs, review evidence, CI,
  release state, and Git topology.
- **Work** — initiatives, tasks, dependencies, readiness, Kanban, DAG,
  timeline, critical path, ownership, evidence, and independently reviewed
  outcomes.
- **Workflows** — versioned definitions, admitted runs, provider negotiation,
  attempts, progress, cancellation, recovery, artifacts, integration, and
  terminal receipts.
- **Automations** — schedules, bounded evidence acquisition, run history,
  proposals, approval/adoption state, and safe cancellation.
- **Observatory** — daemon health, admission, saturation, latency, indexing,
  projection, storage, watcher, queue, and host telemetry with truthful
  denominators.
- **Doctor** — read-only typed diagnosis across configuration, stores,
  projections, hosts, providers, and runtimes. Doctor never repairs or invokes
  a suggested operation.
- **Costs** — bounded usage, budget, model/provider, attempt, and workflow cost
  observations without secrets or high-cardinality metric labels.
- **Settings** — desired/effective configuration, supported capabilities,
  redacted secret references, and explicit validated changes.

Cross-links carry exact project, worktree snapshot, generation, session,
task/run, evidence anchor, and authorized time scope. A link never smuggles a
database path or bypasses the application layer.

## Storage model

Final V2 starts with a fresh profile in the final schema. TraceDecay does not
read, migrate, dual-write, backfill, or consolidate an older TraceDecay
database. Historical information from supported agent hosts remains valuable
and is ingested as ordinary source capture after enrollment.

- SQLite stores relational records, content locators, immutable events,
  receipts, leases, watermarks, desired/effective configuration, and other
  transactional state.
- One embedded Grafeo store per canonical project owner shard stores graph
  topology and vector indexes for code, Git, sessions/LCM lineage, Work,
  workflows, and typed references between those domains.
- Holographic fact content and its FHRR operations remain in the project-wide
  memory authority. Grafeo stores only typed fact identifiers, relations, and
  vectors needed for graph/vector retrieval.
- Linked worktrees share the canonical project store while retaining exact
  worktree snapshot and generation identity.

The dashboard receives only typed application results. It cannot select or
inspect these stores directly.

## Sessions and lossless LCM

LCM summaries are derived navigation nodes, not replacements for source
content. Summary sources are paginated with opaque cursors and hydrate through
the canonical redaction/content authority of each source message. Retention
cannot delete exact source content merely because a summary exists.

Public session and LCM operations are read-only: status, diagnosis, message
search, retrieval, source expansion, temporal evolution, and preflight
planning. Compression, live transcript projection, and session-boundary
mutation run through the daemon hook-runtime lifecycle used by host context
engines. Dashboard reads cause no hidden refresh or access-counter write.

## Doctor and explicit remediation

Doctor reports typed findings such as `healthy`, `degraded`, `partial`,
`stale`, `unavailable`, `unsupported`, `denied`, or `unknown`, together with
bounded evidence and coverage. It may name an independently authorized product
operation that could address a finding, but it does not construct, dispatch,
preview, apply, or verify that operation.

If the user chooses a remediation, the dashboard invokes the owning
application operation explicitly. Effects require authorization, idempotency,
preconditions, cancellation semantics, and a durable receipt. The dashboard
then re-runs the read-only observation to show the result.

## Graph and vector views

Structure views query the daemon's GraphDb application ports. They never
reconstruct graphs from SQL rows in the browser. Results are projection-scoped,
bounded, generation-aware, and explicit about stale, partial, indexing,
unsupported, or unavailable state.

Vector search executes inside the embedded Grafeo authority and hydrates
ranked identifiers through the owning content store. The dashboard never
receives an unbounded embedding table or performs a flat client-side scan.

## Safety and privacy

- All project/profile selection is fail-closed and authorization is rechecked
  on cursor resume and handle dereference.
- Missing, hidden, out-of-scope, and policy-denied resources do not reveal
  alternate identities, counts, timing, paths, or provider state.
- Logs and UI payloads exclude credentials, raw prompts, private source,
  provider frames, and unredacted personal data.
- Cursors and response handles are opaque, bounded, expiring, and scoped.
- Empty, partial, stale, unavailable, unsupported, saturated, cancelled,
  timed-out, failed, and effect-unknown are distinct states.

## Development verification

From `dashboard/`:

```bash
npm run typecheck
npm test
npm run build
npm run contracts:check
```

Rust integration tests must exercise the daemon-backed application journey,
not a browser-to-database shortcut or a synthetic route lookalike. A clean
checkout must build the dashboard before Rust compilation because the assets
are embedded by `build.rs`.
