# TraceDecay V2 Root, Migration, and Cutover

## Status / role

Normative PR19 plan. PR19 performs the complete forward migration from released
V1 data and root wiring, atomically makes V2 authoritative, provides bounded
recovery, and deletes migration-only and superseded V1 paths.

**Memory cutover correction (updated 2026-07-27).** The delivered Memory
V1→V2 branch-store cutover is complete. Its typed owner archive covers all 33
authoritative Memory V2 families; the physical adapters are checked for
one-to-one family parity, archive construction validates referential closure,
imports merge idempotently, and cutover/removal receipts bind the owner,
archive schema, source generation, and archive digest before the old branch
store can be reclaimed. Public reads and consolidation/cutover regressions
exercise the resulting V2 authority. The V1-shaped memory bank remains an
explicitly owned permanent compatibility projection maintained by the
canonical memory transaction; it is not an alternate writer or a superseded
archive. Generic deletion language in this plan does not authorize removing
that compatibility projection. This closure does not complete PR19's broader
released-installation migration, recovery window, or V1 product-path deletion.

**Root lifecycle correction (2026-07-26).** The four former non-test
`TraceDecay::lifecycle` stubs are implemented. Direct project init, project
open, read-only open, and branch open now acquire the exact profile's owned
exclusive maintenance scope and delegate to the registered production
authorities; `configuration_runtime_unavailable` remains only in test-transport
builds.

**Localized integrity-repair correction (2026-07-27).** Bundled-SQLite FTS
blob corruption is now covered by a direct open-and-self-heal regression that
restores search while leaving whole-database corruption fail-closed. This
closes the localized FTS repair regression only; it does not weaken the
maintenance fence or authorize replacement of a generally corrupt database.

**Schema-disposition evidence correction (2026-07-26).** A consolidation
inventory test accepted any non-`None` disposition, so labelling a table
`"merged"` passed without proving corresponding merge SQL existed.
`external_source_states_v1` exposed this false green. A disposition label is
inventory coverage only; it cannot certify migration delivery. Only an
executable source-to-target migration with direct row/identity/semantic
verification satisfies this plan.

Earlier migration fixture names, family inventories, packet layouts, and
intermediate cutover scaffolding are historical evidence, not prerequisites
or artifacts that PR19 must recreate. Published API aliases and persisted data
formats remain compatibility contracts until their declared migration or
retirement completes; acceptance otherwise follows the direct upgrade,
recovery, platform, and regression behavior below.

## Measured crate-extraction sequencing (adjudicated 2026-07-28)

The root crate's compilation wall is the current developer-experience
priority. [Plan 33](33-end-to-end-performance-optimization.md) owns the
reproducible 121.35 s root-check baseline and every baseline-versus-treatment
comparison. This section owns the dependency order and root-compatibility
mechanics; it does not declare that every named boundary is already, or must
become, a package.

The executable slice registry, gates, commit policy, and exact-SHA worktree
procedure are in the canonical
[`docs/superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md`](../../superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md)
and its `v2/` child plans. Those plans apply this section; they do not replace
its leaves-first dependency authority.

The sequencing evidence came from a read-only source scan of `crate::` and
`pub(crate)` edges with `rg`. The TraceDecay MCP server was unavailable and
the equivalent CLI graph calls timed out, so these counts are useful
architecture observations rather than graph-certified dependency proof. Each
slice must leave the workspace compiling, use the measured baseline rather
than package count or line count as a proxy, and retain its crate only if a
same-host comparison shows the frequently touched compile graph improved.

The measured root-module footprint (lines/files, with `src/daemon.rs` called
out separately) explains the leaves-first order:

| Root area | Lines / files |
| --- | ---: |
| `daemon` | 97,022 / 110, plus `src/daemon.rs`: 6,914 |
| `application` | 85,413 / 111 |
| `sessions` | 77,410 / 113 |
| `global_db` | 68,138 / 88 |
| `mcp` | 64,406 / 89 |
| `extraction` | 45,022 / 60 |
| `agents` | 34,343 / 34 |
| `migrate` | 32,290 / 43 |
| `db` | 31,293 / 69 |
| `query` | 30,864 / 46 |
| `dashboard` | 26,818 / 47 |
| `automation` | 24,928 / 47 |
| `store` | 20,896 / 32 |
| `hooks` | 16,471 / 20 |
| `semantic_code` | 14,476 / 11 |
| `code_index` | 13,606 / 19 |

### Sequenced slices

1. **Prepare domain contracts, then extract query first.** Split
   `src/types.rs` by its actual destination: graph/extraction/traversal
   records enter the domain `code_intelligence` vocabulary; edit/move DTOs
   enter application `source_edit`; and `CostTurn` becomes a domain
   observability contract. Make protobuf node variants unconditional domain
   vocabulary to remove the feature-shape cycle. Then move all of
   `src/query/**` (30,864 lines / 46 files) to `tracedecay-query` while the
   root preserves existing paths with `pub use tracedecay_query as query;`.
   The scan found 234 inbound `crate::query` sites in 43 files, but one
   outbound root dependency:
   `query/retrieval/lexical/projection.rs` imports
   `code_index::chunks::ExtractionAdmittedCodeSearchChunkV1`. Move that chunk
   contract to domain first, so query is a clean leaf.
2. **Extract `tracedecay-code-index`.** Move `src/code_index/**` and
   `src/extraction/**` together (58,628 lines / 79 files), including the
   `lite`/`medium`/`full`/`lang-*` features and the WGSL `cc` build step. Keep
   `src/extraction_worker.rs` in root because it owns subprocess lifecycle,
   not deterministic indexing. Split `src/ast_grep_search.rs` so only the
   grammar/pattern/matcher core moves.
3. **Separate pure projector reducers.** Extract roughly 1,709 pure lines:
   all of `sessions/claude/canonical_projection.rs`, deterministic derivation
   from `global_db/observation_projection/apply.rs`, and pure lineage and
   transition-classification symbols. Projectors return effects; adapters
   apply them; no projector receives a database connection.
4. **Move capture by provider family.** After introducing sink, cursor, and
   admission ports, move roughly 36,074 lines / 55 files one provider family
   at a time: Claude; Codex; Cursor/Cline/Kiro/Vibe; Hermes; Cursor Composer;
   and the privacy sanitizer.
5. **Converge application by use case.** Move concrete adapters out before
   moving each use-case family, so application consumes narrow ports rather
   than carrying its current infrastructure with it.
6. **Cut over hooks after their delivery port exists.** Move the 16,471-line
   hooks area only after a hook runtime/delivery port is in place. Root hooks
   presently reach about 16 root modules, so they are not yet the thin
   adapters described by [Plan 07](07-hooks-crate.md).
7. **Cut over HTTP and dashboard one route family at a time.** Keep static
   assets injected by root while routes are moved. This converts the
   [Plan 10](10-api-crate.md) boundary without letting handlers retain
   business or store ownership.
8. **Consolidate the rusqlite runtime last.** Move `src/db/**`,
   `src/global_db/**`, and `src/store/**` (120,327 lines / 189 files) only
   after dependency inversion. This is the highest-risk slice because it
   changes the runtime authority rather than a leaf.

[Plans 05](05-query-crate.md) and
[25](25-code-intelligence-indexing-crate.md) already make their respective
crate extractions optional: same-host compile evidence, dependency isolation,
and reuse must justify each boundary. [Plan 19](19-system-defragmentation-convergence-and-extensibility.md)
and Plan 33 require a baseline-versus-treatment measurement before retaining
each crate. No slice may be presumed beneficial because the root is large.

Plans [03](03-capture-crate.md) and [04](04-projectors-crate.md) specify
capture and projector boundaries, not crate-first framework projects.
`tracedecay-capture` and `tracedecay-projectors` are convenient names inferred
from those plan filenames, not binding package declarations.

### Compatibility, current debt, and extraction risks

The root remains composition, daemon lifecycle, discovery, upgrade handoff,
and a deliberately small compatibility façade. The query re-export is the
first example. If a move needs visibility outside the former root module,
introduce a narrow typed port; do not broadly convert `pub(crate)` internals
to `pub`. The unimplemented API-migration planner/apply journey in
[Plan 34](34-workspace-refactoring-and-api-migration.md) is not an extraction
dependency: ordinary source moves, explicit façades, and narrow ports must
stand on their own.

The review found known layout debt that this sequence must reduce rather than
repackage:

- projector logic is physically interleaved with SQL;
- capture files combine parsing, admission, and concrete persistence;
- root application mixes use cases and infrastructure;
- root dashboard handlers retain business/store logic despite Plan 10;
- `src/store/**` contains adapters, not `tracedecay-store` contract content; and
- two `RequestContext` models remain unconverged, with only the crate model
  carrying `ResolvedScope`.

Before and after every slice, account for these risks:

- Moving DTO ownership can rename generated `schemars` definitions and drift
  the generated dashboard contracts.
- The root `Cargo.toml` `include` whitelist will no longer package moved
  source, so each new crate must package its own fixtures, vendored grammar
  inputs, and benches.
- Moving code-index without its WGSL build ownership leaves the root
  invalidation problem intact.
- A root compatibility façade may need public symbols where callers currently
  use `pub(crate)` visibility; solve that with explicit narrow ports, not
  wholesale visibility widening.

## User outcome

An existing user upgrades without losing supported data or changing the
meaning of queries. After cutover, clients reconnect to one V2 daemon authority
and continue through supported surfaces. A failed or interrupted upgrade either
resumes safely before cutover or rolls forward from verified recovery material
after cutover; it never restores V1 as a writer.

## End-to-end production path

1. The owning daemon acquires a maintenance fence that pauses ingest, sync, and
   other writers for the affected store. Preflight discovers every supported V1
   family, schema/version, source path, destination scope, required space, and
   blocking corruption, and returns an actionable outcome when complete
   migration cannot be proved.
2. Before any source mutation, the daemon creates and verifies a recoverable
   backup. The only usable copy is never overwritten.
3. The daemon imports every detected supported family into isolated V2 staging
   in bounded transactions. Durable checkpoints bind the migration and source
   epochs, family and deterministic range, transform revision, exact
   project/user destination identity, counts, and digest. Restart validates those inputs
   and is idempotent or fails closed.
4. Verification compares identities, references, counts, normalized content
   and content hashes where applicable, normalized query results,
   deletion/correction/quarantine lineage, scope mapping, searchability, and
   representative reads. Missing, corrupt, partial, or noisy proof yields
   `insufficient_evidence` or a typed blocking failure, never an optimistic
   cutover.
5. Cutover atomically publishes the verified V2 store and schema epoch. Before
   that point V1 is the only authority; after it, the V2 daemon is the only
   writer and all CLI, MCP, hook, API, LSP, dashboard, and SDK clients
   reconnect through it rather than opening storage.
6. The verified V1 archive remains read-only for one declared recovery window.
   It records source version, checksum, timestamp, migration ID, and restore
   instructions.
   Recovery restores forward into a new verified V2 epoch, reapplies every
   newer disposition, rebuilds affected derivatives, and fences older daemons.
   There is no reverse cutover, dual write, production shadow read, or lazy
   read migration.
7. When the recovery window and deletion policy are satisfied, PR19 deletes the
   V1 archive and all migration-only code and reports what was removed.

Before PR16 one local daemon owns the live store. With remote shared Brain,
exactly one fenced daemon authority owns each mutable shard; migration never
creates another writer.

The maintenance fence also covers integrity repair, index rebuild, cutover,
and offline recovery so scheduled sync/ingest cannot race verification. An FTS
or projection repair is admitted only after corruption is localized to
deterministically rebuildable derived state; whole-store or authoritative
family corruption remains preserve-and-escalate.

## PR19 implementation defaults

- Use the existing SQLite `VACUUM INTO`/backup path, migration and
  fault-injection seams, and proptest coverage. These replace a new backup
  copier, migration orchestration framework, and separate crash/property
  harness while retaining maintenance fencing, isolated staging, bounded
  transactions, durable checkpoints, semantic verification, atomic cutover,
  and forward-only recovery.
- If a source store or supported platform cannot create and verify the required
  snapshot through those seams, block cutover and use the existing explicit
  verified-backup fallback. Unknown, partial, or unverifiable backup state
  remains a typed failure; do not add a migration framework or treat
  schema-only equality as success.

## Implementation slices

### Complete staged migration

- Ship preflight, verified backup, maintenance fencing, family-by-family
  staging, durable checkpoints, and restart/resume as one callable daemon
  upgrade path.
- Migrate all detected supported families. Unknown or corrupt required data
  blocks the upgrade with Doctor guidance; PR19 has no skipped-family or
  deferred-family success state.
- Classify corruption by family. Deterministically rebuildable indexes and
  projections may be repaired under exclusive maintenance authority;
  authoritative facts, observations, sessions, and receipts are preserved and
  escalated.

### Verify and cut over atomically

- Run semantic-equivalence checks against real reads and searches while V1 is
  still authoritative and any comparison remains read-only.
- Publish one verified V2 epoch atomically, reconnect clients, and reject an
  older or stale client/daemon with an actionable upgrade error.
- Preserve stable public compatibility names only as thin delegates to
  canonical V2 application operations. They own no storage, policy, lifecycle,
  or migration logic.

### Recover forward, then delete

- Validate daemon replacement during upgrade handoff and recover to the last
  verified V2 state if replacement fails.
- Keep the archive only for the bounded recovery policy, including provenance
  required to preserve erasure, correction, quarantine, and derivative
  ownership.
- Block archive eligibility until deletion, correction, quarantine, and
  derivative ownership are captured. Restoration replays every newer
  disposition before serving and rebuilds affected derivatives; provenance
  never overrides erasure.
- Delete obsolete V1 root wiring, direct database clients, temporary adapters,
  dead flags, migration-only dependencies/features/build inputs, and their
  dedicated test support after the recovery boundary passes.

## Replacement and deletion

PR19 removes duplicate transport/admin handlers, handler-local database and
query logic, obsolete root-owned product implementation, writable fallbacks,
external `ast-grep` capability probing, and subprocess outline/rewrite paths.
Temporary compatibility wrappers are deleted after their named consumer has
migrated. Stable published aliases remain, but delegate to the same canonical
operation and preserve equivalent authorization, errors, redaction, effects,
pagination, streaming, cancellation, and retry behavior.

The final root package owns composition, daemon lifecycle, discovery, upgrade
handoff, and stable compatibility entry points. It is not a catch-all product
implementation or a permanent migration runtime.

## Direct acceptance

- Real V1 fixtures containing every supported family migrate through the
  production upgrade entry point and produce semantically equivalent V2 reads,
  searches, identities, references, and lineage.
- Fault injection at every preflight, backup, staging transaction, checkpoint,
  verification, cutover, archive, and forward-recovery boundary proves
  crash/restart behavior and that no partial authority is published.
- Multi-client tests prove one writer, maintenance fencing, reconnect behavior,
  and refusal by older binaries after the schema-epoch fence advances.
- Upgrade-handoff tests quiesce writes, validate the replacement daemon,
  preserve client reconnection, and recover to the last verified V2 state
  without re-admitting an older daemon as writer.
- Linux and Windows upgrade journeys cover clean success, insufficient space,
  required-family corruption, interrupted migration, failed daemon handoff,
  post-cutover forward recovery, and archive expiry.
- Doctor reports actionable preflight, lock, version, corruption, incomplete
  migration, archive, and recovery states and performs only explicitly selected
  safe repairs.
- Ordinary aggregate repository checks pass after the journey tests; no
  separate acceptance gate is created. PR19 leaves no dual-write/read path,
  lazy migration, reverse cutover, direct writable client, skipped family,
  migration TODO, generated inventory, or migration-only implementation.

## Not in PR19

- New product-domain behavior or a second compatibility implementation.
- Autonomous Git history mutation.
- Indefinite archive retention or V1 writer restoration.
- A migration dashboard, execution ledger, schema-only conformance suite, or
  placeholder acceptance baseline.
