# Embedded Grafeo graph and vector authority

## Outcome

TraceDecay embeds [Grafeo](https://grafeo.dev/) in the daemon and uses it as the
sole durable graph-topology and vector-index authority. `tracedecay-graph-db`
is the only workspace crate that depends on Grafeo or exposes its types.
Product crates use typed TraceDecay ports.

This is a completed production cutover, not a sidecar, optional accelerator,
adapter demonstration, stage, or second authority. Every graph/vector caller
moves in the same delivery slice and the superseded SQL/custom implementation
is deleted.

## Global invariants

1. One Grafeo store exists per canonical project owner shard. Linked worktrees
   share it while retaining exact worktree snapshot and generation identity.
2. Branch, ref, worktree, commit, pull request, session, agent, task, and run
   are graph entities/provenance, never database or fact-shard owners.
3. Only the daemon/application composition opens stores. MCP, CLI, HTTP, LSP,
   SDKs, hooks, plugins, and the dashboard never open Grafeo or SQLite.
4. SQLite remains the authority for relational records, immutable events,
   content locators, receipts, leases, watermarks, configuration, and other
   transactional state.
5. Grafeo owns graph topology and vector indexes. No SQL node/edge/vector
   table, in-memory production fallback, flat vector scan, or dual write
   survives.
6. Holographic fact content and intrinsic FHRR operations remain in the
   project-wide memory authority. Grafeo stores only typed fact IDs, relations,
   and vector indexes needed for graph/vector retrieval.
7. Final V2 starts fresh. An old or incompatible TraceDecay store returns
   `ResetRequired`; it is never migrated, backfilled, converted, dual-written,
   or consolidated. Historical host transcripts enter by ordinary capture.
8. Exact/lexical/graph and ordinary retrieval remain usable while semantic
   vectors, historical convergence, or rebuilds are incomplete. Partial,
   indexing, stale, failed, cancelled, and unavailable are typed states.

## Crate boundary

`crates/tracedecay-graph-db` owns:

- embedded engine open/close and canonical path derivation;
- exact `ProjectId + canonical owner shard` registry identity;
- schema/label/property vocabulary and projection namespaces;
- write transactions, rollback, generation activation, and idempotent
  publication;
- bounded node/edge upsert and delete;
- outgoing, incoming, bidirectional, path, DAG, SCC, and topological reads;
- projection-isolated vector upsert/delete/top-k;
- checkpoint, snapshot, compaction, size, and health telemetry;
- typed engine/problem mapping and cancellation/deadline checks.

It exports no raw Grafeo handle, query string, storage path, or backend error.
Product ports use stable domain identifiers and bounded request/result types.

The workspace root pins one reviewed Grafeo release. `cargo tree --invert
grafeo` must show `tracedecay-graph-db` as the only direct workspace consumer.
No second graph library or home-grown traversal/vector engine remains after the
cutover.

## Native Grafeo usage

The adapter uses the pinned Grafeo API according to its actual guarantees:

- persistent single-file storage, WAL recovery, explicit close, and bounded
  native sessions/transactions provide the storage lifecycle;
- native scalar properties and property indexes resolve project, projection,
  generation, entity, relation, and publication identities without scanning
  every generic node or decoding every JSON payload at open;
- native GQL/algorithm traversal supplies path, DAG, SCC, and topological work.
  The TraceDecay boundary adds deterministic ordering, projection isolation,
  authorization, cancellation, and visit/result budgets that the engine API
  does not express;
- each stable vector label/property/dimension/metric has a real Grafeo HNSW
  index. Queries use native equality prefilters for projection and generation,
  bounded `k`/`ef`, and hydrate only the selected IDs;
- vector mutations use an engine path proven to update the HNSW index in the
  same publication boundary. Low-level session property mutation is forbidden
  unless the adapter also maintains the index and proves insert, update,
  delete, rollback, reopen, and concurrent-read visibility;
- bulk and batch APIs are used only inside the same atomic publication
  semantics. A fast non-atomic import API cannot replace the outbox, expected
  frontier, transaction, or activation receipt; and
- Sync durability calls the WAL sync primitive for a commit. Full checkpoint
  or snapshot serialization is a lifecycle/backup operation, never a
  per-commit or read-snapshot implementation.

TraceDecay does not enable a Grafeo feature merely because it exists. The
pinned release's CDC is not a durable recovery journal, so the SQLite outbox
remains the restart authority. BM25/hybrid search does not replace the
evaluated exact/lexical/fusion authorities and must not turn a missing index
into empty success. Quantization is enabled only by the selected evaluated
vector profile. Native backup/PITR output is wrapped by TraceDecay's fenced
cross-store watermark, checksum, fsync, and restore validation. Engine query
timeouts cover query execution; low-level reads still require explicit
TraceDecay cancellation and budgets.

Grafeo lacks a composite unique constraint for TraceDecay's stable identities.
The adapter therefore retains the minimum exact uniqueness, idempotency,
expected-frontier, and pagination metadata needed for product semantics, but
does not duplicate the entire graph, adjacency lists, or vector inventory in
an in-memory `StateCache`.

## Store and projection identity

The daemon registry opens:

```text
<canonical project owner shard>/graph/graph.grafeo
```

The path is an implementation detail and contains no version, branch, stage,
PR, or domain name. Registry keys use the exact project ID and canonical owner
shard path; aliases cannot create another writer lane or another graph.

Every record includes:

- canonical project ID;
- projection/domain kind;
- stable domain entity ID;
- source generation or immutable version;
- worktree snapshot/commit identity when relevant;
- freshness and publication state;
- provenance/authorization references needed by the owning application read.

Projection namespaces isolate code, Git, session/LCM, Work, workflow, memory
references, automation, and cross-domain edges while allowing explicitly typed
joins. A vector search must select one projection and compatible vector model
generation; it never scans unrelated vectors.

## Publication and consistency

An owning projector prepares a bounded generation outside the active read
frontier, then publishes graph/vector batches transactionally. Activation uses
an exact expected frontier and receipt. Readers freeze one compatible complete
generation; they never observe half a projection or silently combine
incompatible generations.

Where one operation also commits SQLite state:

1. immutable source/event state commits to SQLite with an outbox/publication
   intent and exact expected identities;
2. the Grafeo projector applies an idempotent transaction;
3. activation and the SQLite receipt/watermark reconcile by compare-and-swap;
4. restart resumes from the durable intent; and
5. a failed or cancelled projection remains typed and cannot replace the last
   complete generation.

This is not a permanent dual authority: SQLite retains the source relational
event/receipt, while Grafeo exclusively owns the derived topology/vector.

## Code intelligence

The code index publishes stable symbols, files, modules, types, occurrences,
and typed relations such as calls, imports, implements, inherits, contains,
depends-on, test-covers, diagnostics, and anchored evidence.

Production navigation, hierarchy, callers/callees, impact, dependency,
affected-test, context, graph explorer, and query fusion paths read the shared
project GraphDb. The daemon scheduler injects the persistent handle; no
`Memory` engine is used in production.

Code generations are project-wide and generation-scoped. A worktree selector
chooses the exact indexed snapshot; branch creation/deletion never clones or
deletes a database. Superseded custom adjacency, branch graph lifecycle, and
SQL graph tables/callers/tests are removed.

## Git topology and evidence

Git ingestion uses `gix`/native Git authority for repository identity, refs,
commits, parents, trees, changes, worktrees, and exact snapshot evidence.
Grafeo stores commit/ref/worktree topology and typed links from changes/hunks
to code symbols, tasks, sessions, findings, tests, CI, reviews, and releases.

Git status/diff/blame/content remain hydrated from the owning Git/content
authority. Grafeo does not become a Git object database or fabricate working
tree state. Custom durable adjacency and SQL graph copies are deleted.

Repository admission performs only a native open plus current identity/ref-tip
snapshot. It enqueues graph convergence and returns without history traversal.
The convergence owner keeps durable per-ref/OID frontiers, uses the native
commit graph and bounded object caches, hides already-published tips, and
publishes changed refs and unseen commits in bounded continuation batches.
Each committed batch advances its restart watermark before yielding, so
cancellation or restart resumes without a full rescan. Ref deletion removes
only the corresponding ref topology; immutable reachable commit evidence is
retained until ordinary graph retention proves it unreachable.

Committed-history convergence and dirty-worktree capture are independent.
Filesystem events coalesce into a bounded dirty snapshot keyed by exact
worktree identity; they do not trigger a full ref walk, whole-history digest,
projection replacement, or one database read per historical commit. Foreground
MCP, hook receipt, and exact Git reads have scheduler priority over convergence.

## Sessions and lossless LCM

SQLite/content authorities retain sessions, turns, message occurrences,
redaction state, exact source payload locators, capture watermarks, and other
relational state.

Grafeo stores:

- session/thread/agent relations;
- summary predecessor/successor DAG;
- summary-to-source edges;
- logical copy/continuation/boundary relations;
- typed links to facts, code, Git, tasks, workflows, and evidence.

An LCM summary never replaces exact source content. Paginated summary-source
retrieval uses an opaque cursor and hydrates each source through its owning
redaction/content authority. Cross-project selectors open the selected
registered project's exact store; they never alias the active project graph.

Public LCM reads remain read-only. Compression, live projection, and session
boundaries enter through the daemon hook-runtime lifecycle. If a host does not
expose compaction content, the daemon context engine may create an auxiliary
summary from authorized visible source messages while preserving lineage.

## Semantic vectors

Grafeo is the only durable vector-index/search authority for code, sessions,
tasks, workflow evidence, and typed memory references. Publication records the
model, dimensions, content digest, projection, source generation, and stable
entity ID.

Queries perform bounded top-k search inside the compatible projection and
hydrate results by typed ID through the owning content/relational authority.
Missing, stale, incompatible, redacted, or unauthorized hydration is explicit;
no empty-success fabrication or full-table fallback is allowed.

Exact and lexical tiers are independent and non-demotable. Fusion consumes
typed vector outcomes only when compatible/ready, preserves evidence and
coverage, and reports semantic omission truthfully.

## Holographic memory references

Facts are project-wide. Their content, trust, contradictions, temporal
semantics, retention, and FHRR bind/bundle/similarity operations stay in the
memory authority.

Grafeo may index:

- canonical fact-ID vectors;
- fact-to-fact typed relations;
- provenance edges to branch/ref/worktree/commit/PR/session/agent;
- references to code, Git, tasks, workflows, findings, and LCM nodes.

Deleting a branch/worktree only removes unreachable derived provenance. It can
never delete, move, archive-merge, or shard a fact. There is no legacy
`memory_facts` mirror, cutover receipt, or branch-only fact fixture.

## Work and workflow topology

SQLite retains immutable work/workflow events, payloads, attempts, leases,
budgets, fences, activation CAS, watermarks, receipts, and provider artifacts.

Grafeo exclusively owns:

- Work parent/dependency/blocking/supersession/evidence/ownership topology;
- versioned workflow DAG nodes and edges;
- task/run/attempt/provider/artifact/integration relations;
- readiness, topological order, cycle/SCC, critical path, reachability, and
  bounded cross-domain projections.

Every dashboard, MCP, CLI, HTTP, SDK, automation, placement, scheduler, and
review caller uses these production ports. SQL topology tables and traversal
implementations are deleted after all callers move.

## Cross-domain journeys

The shared graph must support real application journeys, including:

- a changed Git hunk → code symbols/callers → affected tests/diagnostics →
  task/review/CI evidence;
- an LCM summary → paginated exact sources → session/agent → code/Git/task/fact
  context;
- a project fact → provenance → supporting/contradicting sessions/code/Git
  evidence;
- a work item → dependencies/readiness → admitted workflow run → attempts,
  artifacts, integration, and independently reviewed outcome;
- an automation proposal → source evidence → task/workflow execution →
  adoption/rollback observations.

MCP, CLI, HTTP, LSP extensions, Rust/TypeScript SDKs, dashboard, hooks, and host
bundles reach these journeys through canonical application operations. No
surface has a private graph adapter.

## Lifecycle, backup, and telemetry

Daemon startup opens the registry lazily and shares one `Arc<GraphDb>` per
exact project authority. Shutdown stops admission, drains/cancels bounded
operations, checkpoints SQLite and Grafeo, and closes handles without leaving
the writer lane busy.

Backup captures a fenced SQLite snapshot and matching Grafeo checkpoint under
one profile/project/store identity and watermark. Restore validates both before
activation and rejects mixed generations. Rebuilds use final-V2 source
authorities; they do not import an older TraceDecay store.

Bounded telemetry includes open/close/checkpoint/compaction latency, size and
reclaimable bytes, active generations, projection backlog, vector counts/model
generation, transaction conflicts, writer-lane wait, cancellation, and typed
partial/unavailable coverage. Doctor reads this state without acquiring a
writer or performing maintenance.

## Deletion requirements

The cutover is incomplete while any production caller, schema, test fixture,
or active documentation retains:

- `petgraph` or another duplicate graph authority;
- SQLite node/edge/vector topology or flat semantic scan;
- in-memory production GraphDb fallback;
- per-branch graph/database creation, clone, fallback, archive, or deletion;
- branch-only fact storage or fact archive merge;
- direct store access from a surface, hook, host, or SDK;
- V1/V2 migration, backfill, consolidation, parity, or dual-write machinery;
- stage/PR/version module names, compatibility façades, or feature flags hiding
  unfinished production behavior.

Final SQLite tables that store relational events/content/receipts remain. Tests
are recontracted to behavior rather than preserving deleted implementation
shape.

## Verification

Acceptance uses direct behavioral evidence:

- restart/durability and transaction rollback for every projection family;
- one-store identity across linked worktrees and concurrent daemon callers;
- exact generation isolation and stale/incompatible handling;
- bounded incoming/outgoing/path/DAG/SCC/topological/vector reads;
- vector publication/query/hydration with no SQL or flat fallback;
- lossless paginated LCM source hydration;
- project-wide fact survival across branch/worktree deletion;
- Work/workflow readiness, cycle rejection, activation, recovery, and receipt
  reconciliation;
- Git/code/task/session cross-domain journeys through each public surface;
- measured cold admission, warm admission, one-commit advance, force-move,
  ref deletion, dirty-worktree, cancellation, and restart-resume workloads;
- proof that admission performs no history walk, one-commit work scales with
  the delta, every background batch yields, and persisted progress prevents a
  restart from replaying completed history;
- backup/restore fencing and shutdown checkpointing;
- clean-checkout workspace/all-feature/dashboard/host/package/CI suites;
- measured workload comparisons under real deadlines, with distributions and
  no raised timeout or weakened semantics.

Generated artifacts are regenerated from their canonical authority. `dist` and
dashboard build directories remain ignored. Verification never uses the
operator's installed TraceDecay profile and never dogfoods the in-development
daemon.
