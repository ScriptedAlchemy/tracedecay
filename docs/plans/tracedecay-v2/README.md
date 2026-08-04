# TraceDecay V2 rewrite

Status: active product rewrite. PR8 is complete. PR9/PR10 retrieval and
PR12/PR13 production integration are active. PR14 has an implemented
checkpoint but remains open on the named Plan 11 journeys and on stable active
contracts, direct tests, and normal CI. [NEXT.md](NEXT.md) records the current
product outcomes and blockers.

[00-plan-set-index.md](00-plan-set-index.md) is the sole precedence, rejection,
delivery, and acceptance authority. [NEXT.md](NEXT.md) tracks current outcomes
and blockers only; the gap ledger and PR9 contract spine are historical.
These are contributor documents only and never product runtime input.
Numbered plans define component behavior and boundaries, not separate
crate-first work queues.

`TraceDecay V2` is the product-roadmap name, not evidence that every contract
or schema needs a V2 shape. A compatibility alias, deprecation path, dual
reader/writer, contract/schema V2 or V3, or migration is required when the
predecessor exists on `origin/master`, in a published package/release, an
independently deployed client, a live host installation, or a live persisted
format. Branch-local shapes, PR order, historical type names, tests, and future
consumers are not publication evidence. Pure source-only/internal contracts
change to their final shape in place. Wire-visible revisions remain negotiated
until an authorized installed-client/host census proves absence. Anything
potentially installed or written to a store, spool, file, or persisted
projection fail-closes as live and preserves compatibility, backward-read, and
migration/recovery until the applicable authorized census proves absence. A
`V1` suffix may name an initial final wire format without requiring a sibling
version by itself.

## Current product foundation

- `tracedecay-domain` contains the first executable V2 domain contracts.
- `tracedecay-store` defines canonical transcript persistence while the
  already-open `GlobalDb` remains the physical connection and transaction
  authority.
- Transcript ingest, startup catch-up, restart recovery, daemon, MCP, and
  dashboard paths use that authority without a fallback writer.
- Transcript batches atomically update messages, projections, durable cursors,
  and monotonic offsets. Replay and exact retries are idempotent.
- Transcript and LCM mutations use fresh RAII transactions. Failure or
  cancellation rolls back database rows and newly created payload files.
- Direct tests cover Claude, Cursor, Cline-like input, partial records, replay,
  rollback, restart, concurrency, and Windows behavior.
- Existing Doctor, daemon, storage, hooks, MCP, and CLI remain product code.
- Claude production capture now emits path-independent sanitized observations,
  typed receipts, durable cursors, and deterministic searchable projections.
- Observation, receipt, cursor, enqueue, projection effects, and checkpoints
  preserve atomic restart/retry behavior; exact no-op replay performs no writes.
- The committed PR5 workload and checked-in executed benchmark result record the
  production parse/sanitize/commit/project/replay baseline for PR20.
- PR6 extends that path across the supported Claude, Codex, Cursor, Hermes,
  Kiro, and Cline-family sources through one host-neutral catalog and provider
  observation contract.
- Non-replayable events use bounded daemon-owned host admission; replayable
  sources use bounded fair scheduling, atomic cursor/projection commits, and a
  staged bounded projection rebuild rather than provider-local durable state.
- Executable native host fixtures and typed hook-telemetry readiness now replace
  prose-only provider claims. Commit `05da230e` records the historical PR6
  Linux benchmark measurement; PR6 is complete.

## Storage and authority

- One local daemon is the sole mutable SQLite authority. Hooks, clients,
  workers, MCP servers, and dashboard handlers send typed operations to it.
- Project facts and project session/LCM data live in one canonical project-wide
  store shared across branches and worktrees.
- Profile-wide user activity lives in the user/profile store.
- Only code indexes are branch/worktree/snapshot scoped.
- Worktrees resolve their project through the project registry and Git common
  directory. Missing or ambiguous authority fails closed.
- No path may create a worktree-local, source-adjacent, in-memory, recovery, or
  direct-database fallback writer.

## Delivery rules

Every PR13–PR20 section in
[the authoritative roadmap](00-plan-set-index.md) is implemented in this order:

1. Start with the user outcome.
2. Trace one real supported input through daemon/application behavior and
   durable state or computation to an observable result.
3. Add only the implementation slices needed to complete that path.
4. Delete legacy or duplicate paths once the production route and recovery
   boundary permit it.
5. Accept the PR with a direct product journey, focused failure/recovery
   behavior, and normal CI.
6. Defer only work named in that PR's **Not in this PR** section.

Contracts, schemas, adapters, ports, dispatch, packaging, instrumentation, and
tests are part of the first production journey that uses them. They are not
standalone milestones. Component plans contribute requirements to the
authoritative PR sequence; they do not create independent queues, crates, owner
maps, or conformance projects.

Each mechanism has one typed production kernel. Surfaces and compatibility
names translate and delegate; they never own alternate query, edit, storage,
rendering, health, policy, scheduling, measurement, remediation, or recovery
logic. Long operations expose typed progress and explicit cancellation.

Acceptance uses direct behavior and checked-in real provider/data fixtures.
Generated declarations, schema equality, successful client compilation,
inventories, placeholder baselines, and agreement between planning artifacts do
not prove product behavior. Performance instrumentation ships with the
production path it measures, and optimization keeps only reproducible practical
gains with equivalent results.

For completed slices, historical type names, file layouts, fixture names,
milestones, and gates are implementation evidence rather than rebuild
obligations. A deleted or renamed mechanism does not mean the feature is
missing. Before reporting a gap, later audits must map each retained product,
semantic, migration, and recovery requirement to the current canonical owner
and direct behavior/regression evidence. Only missing callable behavior or a
missing direct regression is a gap.

High-confidence architecture and explicit rejection findings are normative.
Medium-confidence models, algorithms, topology choices, renderers, ranking
profiles, calibration methods, and performance mechanisms remain versioned
measured candidates. Low-confidence or causal product-effect claims are not
requirements or acceptance criteria without direct TraceDecay intervention
evidence. Optional features remain disabled until direct product tests and the
applicable developer evaluation justify activation.

Every product quantifier preserves its raw value, unit, denominator, coverage,
cohort, temporal delta, provenance, uncertainty kind, and calibration validity.
Similarity, rank, centrality, and heuristic values are not probabilities, and
no universal quality, health, reward, readiness, or performance score becomes
product truth.

The practical safety baseline is centralized in
[00-plan-set-index.md](00-plan-set-index.md): no credential, prompt, private
source, or provider-payload leakage in logs/fixtures; exact project/user
isolation; authentication at actual network boundaries; and
confirmation/rollback/CAS for destructive Git, host, or protected
configuration operations. Numbered plans keep only the operation-local check
that protects a real boundary and attach it to direct product acceptance.
The authoritative acceptance rule in
[00-plan-set-index.md](00-plan-set-index.md) applies to every active and
historical plan.

PR12 transport and PR18 SDK contracts prove structural, semantic, and lifecycle
compatibility against supported old and current consumers evidenced by
`origin/master`, a published package/release, an independently deployed client,
or a live host installation. Potentially installed branch-era consumers remain
in the compatibility set until an authorized installed-client/host census
proves absence. Direct fault, restart, concurrency, cross-platform, migration,
recovery, and deletion tests remain part of the product journey they protect.

Product, contributor, CI, release, and publication behavior preserves stock
Cargo semantics. A slice that materially changes crate boundaries, dependency
fan-in, feature activation, build-script inputs, or test-target topology records
same-host warm incremental/no-op and representative touched-test evidence,
including wall time, rebuilt units, available CPU/peak-memory data, and visible
variance. Machine-specific timings are diagnostic, not portable thresholds;
Rust Analyzer ownership, local wrappers, lanes, target paths, and cache
placement never become repository or hosted-CI requirements.

Long-running product operations never acquire an automatic no-progress timeout,
rewrite, or hidden agent decision. Real product generation is allowed when it
removes duplicate authorities and follows
[RUST-METAPROGRAMMING.md](RUST-METAPROGRAMMING.md). Planning documents,
inventories, registries, matrices, and generated declarations remain
non-product and never justify implementation or CI by themselves.

The current executable slice is always [NEXT.md](NEXT.md). This roadmap is
contributor documentation, never daemon input, workflow input, product state, or
a source of completion truth. Draft PR #421 merges only after PR20, direct
product tests, and normal cross-platform CI are stable.

## Release

V2 library crates publish through the workspace release flow while the root
package owns the Git tag and GitHub release. A new crate's first crates.io
publication may require one-time trusted-publisher or token bootstrap; this is a
release setup step, not an alternate development workflow.
