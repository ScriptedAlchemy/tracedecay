# Embedded Grafeo Graph Database Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace custom adjacency structures and graph-shaped SQLite storage with one embedded Grafeo runtime boundary while retaining SQLite only for genuinely relational, transactional, and content-bearing records.

**Architecture:** Task 1 retains PR487's temporary direct Grafeo dependency in `tracedecay-query`; Task 2 creates the opaque `tracedecay-graph-db` boundary without prematurely deleting that seed; Task 3 moves every PR487 caller behind the boundary and makes `tracedecay-graph-db` the only workspace crate allowed to depend on Grafeo. Domain crates keep typed TraceDecay identities and contracts; storage adapters translate those contracts into labels, typed edges, properties, vectors, traversals, and snapshots without exposing Grafeo types. Each migrated datum has exactly one authority: durable graph-shaped state lives in Grafeo, rebuildable projections are recreated from their canonical manifests/events, and SQLite does not dual-write or shadow the same graph.

**Tech Stack:** Rust 2024, embedded in-process Grafeo `0.5.42`, TraceDecay domain/store ports, Tokio cancellation, Criterion, cargo-nextest.

## Global Constraints

- Use Grafeo embedded in-process. No server, sidecar, network transport, or separately managed database process.
- Task 3 completes the sole-dependency cutover: until then, only
  `tracedecay-graph-db` and Task 1's unchanged `tracedecay-query` seed may have
  direct `grafeo-*` dependencies. Do not delete or disguise the Task 1
  dependency during Task 2.
- Do not introduce `petgraph` or another overlapping graph/vector database.
- V2 is a breaking fresh-profile cutover: no V1 reader, migration, backfill, compatibility table, dual-write, fallback, or cutover receipt.
- One datum has one authority. A rebuildable Grafeo projection records its source generation/watermark and never becomes a second canonical copy.
- Event-sourced domains use one crash-safe publication protocol: commit the canonical event and an idempotent graph outbox record in SQLite, apply the graph batch, then advance the graph watermark. No caller reads past the acknowledged watermark, and replay never invents a second event.
- Durable facts remain project-wide. Branches and worktrees never own, copy, merge, or retire facts.
- Preserve typed TraceDecay IDs at every boundary; Grafeo node and edge IDs are storage-local handles only.
- Preserve typed cancellation, staleness, denial, unavailable, reset-required, corruption, and budget-exhaustion outcomes.
- Validation and pre-commit mutation failures leave the prior graph readable. A Grafeo post-commit WAL/checkpoint failure is reported as typed `DurabilityUncertain`, permanently closes that handle, and permits no further reads until exact reopen/recovery validates the store.
- Preserve deterministic ordering, pagination, authorization, coverage, and exact source hydration above the storage layer.
- Never install, dogfood, restart, or test V2 against the operator's live TraceDecay profile. All runtime tests use isolated temporary home/profile/socket paths.
- Each implementation lane uses its own recognized worktree, merges the current integration floor before review, and is parent-reviewed before merge.

Official implementation references:

- [Grafeo repository and crate layout](https://github.com/GrafeoDB/grafeo)
- [Embedded Rust API](https://grafeo.dev/user-guide/rust/)
- [Vector and hybrid search](https://grafeo.dev/user-guide/vector-search/)
- [Published Rust API for `0.5.42`](https://docs.rs/grafeo/0.5.42/grafeo/)

---

## Authority map

| Workspace crate | Grafeo adoption |
|---|---|
| `tracedecay-agent-hosts` | No storage dependency. Continue through application/tool APIs. |
| `tracedecay-api` | Wire contracts only; no Grafeo types or handles. |
| `tracedecay-application` | Compose typed graph/vector ports, authorization, hydration, and budgets. |
| `tracedecay-automation` | Consume Work/workflow graph operations; no direct store access. |
| `tracedecay-capture` | No change; observations remain canonical input. |
| `tracedecay-code-extraction` | No change; extracted typed relations remain canonical input. |
| `tracedecay-code-index` | Publish immutable code graph and vector generations through graph-db adapters. |
| `tracedecay-dashboard-api` | Consume application read models only. |
| `tracedecay-domain` | Own IDs, edge kinds, vectors, requests, outcomes, and validation; no database dependency. |
| `tracedecay-global-db` | Keep registry/configuration/observation/session content tables; move session hierarchy and temporal relation projections to graph-db. |
| `tracedecay-host-integration` | No storage dependency. |
| `tracedecay-hooks` | No storage dependency. |
| `tracedecay-jsonrpc` | No storage dependency. |
| `tracedecay-lsp` | Consume application query ports only. |
| `tracedecay-migrate` | Delete graph/memory migration and consolidation residue; V2 creates final stores only. |
| `tracedecay-policy` | Remain pure over typed inputs. |
| `tracedecay-query` | Consume graph-db-backed ports; remove direct Grafeo dependencies after PR487 lands. |
| `tracedecay-runtime-core` | Compose graph-db, code-index, memory, and SQLite authorities; stop owning graph SQL. |
| `tracedecay-sdk` | Public host-neutral contracts only. |
| `tracedecay-search-eval` | Exercise public query behavior and quality; no direct store access. |
| `tracedecay-semantic` | Produce verified embeddings; persist/search admitted vectors through graph-db. |
| `tracedecay-sessions` | Move LCM/source/successor/logical-copy/thread/agent DAG relations to graph-db; retain raw content and replay/retention journals in SQLite. |
| `tracedecay-temporal-query` | Consume typed session graph ports. |
| `tracedecay-rusqlite-parity` | Delete graph/vector fixtures and probes after cutover; retain SQLite parity for retained relational stores. |
| `tracedecay-rusqlite-runtime` | Delete the `graph` module and graph-shaped Work/workflow SQL after callers move; retain connection, ledger, repository, receipt, idempotency, and relational transaction support. |
| `tracedecay-sqlite-parity-protocol` | Remove graph/vector parity variants; retain relational protocol variants. |
| `tracedecay-store` | Own graph-db-neutral attachment, snapshot, generation, and operation ports. |
| `tracedecay-tool-catalog` | No storage dependency. |
| `tracedecay-usecases` | Replace Git/vector SQL adapters with typed graph-db application adapters. |
| Root `tracedecay` crate | Wire one daemon-owned graph-db registry and expose only typed application journeys. |

## Data placement

Move to Grafeo as sole authority:

- code symbol/file/chunk nodes, canonical relation edges, graph traversal indexes, and admitted code vectors;
- Git repository/ref/commit/parent topology and typed commit-to-code/session/work evidence relations;
- Work-item/dependency/current-version topology and rebuildable projections over immutable Work events;
- workflow-definition DAGs and rebuildable run/attempt topology;
- LCM summary/source/successor, logical-copy, thread, and agent hierarchy relations;
- memory fact/entity/assertion relations, holographic vector banks, semantic vectors, and graph/vector indexes; and
- cross-domain relation locators used for bounded authorized traversal.

Keep in SQLite:

- registry, configuration, secrets metadata, observation admission, source cursors, inbox/outbox, idempotency, effects, leases, receipts, and transactional journals;
- raw session/message content, external payload references, redaction authority, exact evidence spans, and retention/GC journals;
- immutable Work/workflow event payloads, runtime fencing, execution receipts, and artifact metadata;
- embedding model manifests, acquisition/install state, generation publication state, and exact source manifests; and
- telemetry/event accounting and other relational aggregates that do not need graph traversal or vector similarity.

## Task 1: Integrate PR487 as the reviewed seed

**Files:**
- Merge: branch `codex/grafeo-code-graph` / PR487 into `codex/tracedecay-total-redesign-plan`
- Verify: `crates/tracedecay-query/src/retrieval/graph/projection.rs`
- Verify: `crates/tracedecay-query/src/retrieval/graph/tests.rs`
- Verify: `crates/tracedecay-query/src/retrieval/graph/tests/measurement.rs`
- Verify: `crates/tracedecay-query/src/retrieval/graph/tests/scale.rs`

**Interfaces:**
- Consumes: `CanonicalRelationEdgeV1`, `CodeSearchChunkV1`, `GraphLaneRequest`.
- Produces: behavior-preserving Grafeo traversal inside `CodeGraphEvidenceAdapterV1`; this is temporary direct coupling removed by Task 3.

- [ ] **Step 1: Merge the current integration floor into PR487's worktree**

Run:

```bash
git -C /home/zack/.codex/worktrees/7e48/tracedecay fetch origin
git -C /home/zack/.codex/worktrees/7e48/tracedecay merge --no-edit codex/tracedecay-total-redesign-plan
```

Resolve only real overlapping production intent. Regenerate `Cargo.lock`; do not choose a side wholesale.

- [ ] **Step 2: Prove PR-specific behavior**

Run:

```bash
cargo nextest run -p tracedecay-query graph --no-fail-fast
cargo clippy -p tracedecay-query --all-targets --all-features -- -D warnings
```

Expected: non-zero graph test count, deterministic traversal, bounded depth and candidate budgets, no PR487-introduced Clippy failures.

- [ ] **Step 3: Parent-review the complete diff**

Review:

```bash
git -C /home/zack/.codex/worktrees/7e48/tracedecay diff --check codex/tracedecay-total-redesign-plan...HEAD
git -C /home/zack/.codex/worktrees/7e48/tracedecay diff --stat codex/tracedecay-total-redesign-plan...HEAD
```

Reject unbounded path growth, hidden panics, nondeterministic ordering, duplicate graph authority, or failures unique to PR487. Inherited PR421 failures remain tracked by their owning lanes.

- [ ] **Step 4: Merge the reviewed PR branch**

Run from the integration worktree:

```bash
git merge --no-ff codex/grafeo-code-graph -m "merge: seed embedded Grafeo code graph"
git push origin codex/tracedecay-total-redesign-plan
```

## Task 2: Create the `tracedecay-graph-db` boundary

**Files:**
- Create: `crates/tracedecay-graph-db/Cargo.toml`
- Create: `crates/tracedecay-graph-db/src/lib.rs`
- Create: `crates/tracedecay-graph-db/src/error.rs` with `Cancelled`, `InvalidRequest`, `Conflict`, `BudgetExhausted`, `ResetRequired`, `Corrupt`, `Unavailable`, `DurabilityUncertain`, and `Closed`
- Create: `crates/tracedecay-graph-db/src/location.rs`
- Create: `crates/tracedecay-graph-db/src/runtime.rs`
- Create: `crates/tracedecay-graph-db/src/projection.rs`
- Create: `crates/tracedecay-graph-db/src/publication.rs`
- Create: `crates/tracedecay-graph-db/src/traversal.rs`
- Create: `crates/tracedecay-graph-db/src/vector.rs`
- Create: `crates/tracedecay-graph-db/tests/runtime_contract.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: validated opaque TraceDecay namespace, projection, entity, relation, generation, watermark, and vector identities.
- Produces:

```rust
pub struct GraphDb;
pub struct GraphSnapshot;
pub struct GraphWriteBatch;
pub struct GraphPublication;

pub struct GraphDbOpenOptions {
    pub location: GraphDbLocation,
    pub expected_format: GraphFormatVersion,
    pub durability: GraphDurability,
    pub cancellation: CancellationToken,
}

pub struct GraphPublication {
    pub namespace: GraphNamespace,
    pub idempotency_key: GraphIdempotencyKey,
    pub source_generation: SourceGeneration,
    pub expected_watermark: Option<GraphWatermark>,
    pub next_watermark: GraphWatermark,
    pub batch: GraphWriteBatch,
}

impl GraphDb {
    pub fn open(options: GraphDbOpenOptions) -> Result<Self, GraphDbError>;
    pub fn snapshot(&self) -> Result<GraphSnapshot, GraphDbError>;
    pub fn apply(&self, batch: GraphWriteBatch) -> Result<GraphCommit, GraphDbError>;
    pub fn replace_projection(
        &self,
        replacement: ProjectionReplacement,
    ) -> Result<GraphCommit, GraphDbError>;
    pub fn publish(
        &self,
        publication: GraphPublication,
    ) -> Result<GraphCommit, GraphDbError>;
    pub fn traverse(
        &self,
        request: TraversalRequest,
    ) -> Result<TraversalResult, GraphDbError>;
    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError>;
}
```

- [ ] **Step 1: Write failing runtime-boundary tests**

Cover in-memory and persistent open, exact-final format validation, atomic batch rollback, snapshot isolation, deterministic traversal, cancellation, vector dimension/metric rejection, reopen durability, corruption, and foreign-shape `ResetRequired`.

Run:

```bash
cargo nextest run -p tracedecay-graph-db --no-fail-fast
```

Expected: fail because the crate and interfaces do not exist.

- [ ] **Step 2: Add the workspace crate and centralize Grafeo dependencies**

Use the exact PR487-compatible Grafeo `0.5.42` dependency set in the new crate.
Add `tracedecay-graph-db` to `[workspace].members` and centralize the exact
versions in `[workspace.dependencies]`. Preserve Task 1's direct
`tracedecay-query` dependencies unchanged until its production callers move in
Task 3; Task 2 adds no other direct Grafeo consumer.

- [ ] **Step 3: Implement typed open, snapshot, batch, traversal, and vector adapters**

Keep Grafeo IDs private:

```rust
pub struct GraphEntity {
    pub identity: GraphEntityId,
    pub labels: BTreeSet<GraphLabel>,
    pub properties: BTreeMap<GraphPropertyName, GraphProperty>,
}

pub struct GraphRelation {
    pub identity: GraphRelationId,
    pub from: GraphEntityId,
    pub to: GraphEntityId,
    pub kind: GraphRelationKind,
    pub properties: BTreeMap<GraphPropertyName, GraphProperty>,
}
```

All validation occurs before mutation. Validation and transaction failures leave the prior generation readable. Grafeo `0.5.42` surfaces some WAL/checkpoint failures after its in-memory commit; those failures poison the handle as `DurabilityUncertain` instead of falsely claiming rollback or serving uncertain state. `GraphPublication` carries the canonical event/generation identity, idempotency key, expected graph watermark, replacement batch, and resulting watermark; same-key/same-input replay returns the original commit while changed input conflicts.

- [ ] **Step 4: Verify boundary isolation**

Run:

```bash
rg -n 'grafeo' --glob 'Cargo.toml'
cargo nextest run -p tracedecay-graph-db --no-fail-fast
cargo clippy -p tracedecay-graph-db --all-targets --all-features -- -D warnings
```

Expected: direct Grafeo consumers are limited to
`crates/tracedecay-graph-db/Cargo.toml` and the unchanged Task 1
`crates/tracedecay-query/Cargo.toml` seed; the graph-db API exposes no Grafeo
types; all tests and Clippy pass. Sole-manifest ownership is Task 3 acceptance,
not Task 2 acceptance.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tracedecay-graph-db
git commit -m "feat(graph-db): add embedded Grafeo runtime boundary"
```

## Task 3: Refactor PR487 behind graph-db and cut over code graph

**Files:**
- Modify: `crates/tracedecay-query/Cargo.toml`
- Modify: `crates/tracedecay-query/src/retrieval/graph/projection.rs`
- Modify: `crates/tracedecay-query/src/retrieval/graph/tests.rs`
- Modify: `crates/tracedecay-code-index/Cargo.toml`
- Create: `crates/tracedecay-code-index/src/graph_projection.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/registry/ports.rs`
- Delete after callers move: `crates/tracedecay-rusqlite-runtime/src/graph/attachment.rs`
- Delete after callers move: `crates/tracedecay-rusqlite-runtime/src/graph/fixtures.rs`
- Delete after callers move: `crates/tracedecay-rusqlite-runtime/src/graph/locator.rs`
- Delete after callers move: `crates/tracedecay-rusqlite-runtime/src/graph/mod.rs`
- Delete after callers move: `crates/tracedecay-rusqlite-runtime/src/graph/mutation.rs`
- Delete after callers move: `crates/tracedecay-rusqlite-runtime/src/graph/read.rs`
- Delete after callers move: `crates/tracedecay-rusqlite-runtime/src/graph/tests.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/lib.rs`

**Interfaces:**
- Consumes: one immutable code generation, its chunks, symbols, files, typed relation edges, and admitted vector generation.
- Produces: `CodeGraphProjectionPublisher`, `CodeGraphEvidenceReadPort`, and frozen graph snapshots keyed by `CodeGenerationId`.

```rust
pub trait CodeGraphProjectionPublisher {
    fn publish_code_graph(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        cancellation: &CancellationToken,
    ) -> Result<GraphWatermark, RetrievalPortError>;
}
```

- [ ] **Step 1: Add failing reopen, generation-isolation, and traversal-equivalence tests**

Test the PR487 fixture through a graph-db-backed production adapter. Assert exact path segments, authority weakening, coverage, unknown targets, max depth, cancellation, and identical ordered output before/after reopen.

- [ ] **Step 2: Publish code generations atomically**

Build the replacement off to the side, validate every typed identity, then replace the generation pointer in one graph-db commit. Readers keep the prior complete generation until publication succeeds.

- [ ] **Step 3: Remove direct query-to-Grafeo coupling**

`tracedecay-query` imports only the consumer-owned `CodeGraphEvidenceReadPort`. Move graph construction into `tracedecay-code-index`; graph-db performs storage/traversal. Remove all `grafeo-*` dependencies from `tracedecay-query`.

Task 3 cutover acceptance requires:

```bash
rg -n 'grafeo' --glob 'Cargo.toml'
```

Expected: Grafeo appears only in
`crates/tracedecay-graph-db/Cargo.toml`. Do not claim the sole dependency
boundary before this production caller cutover and manifest deletion are both
complete.

- [ ] **Step 4: Delete the SQLite graph adapter**

After production registry and read/write callers use graph-db, delete the complete `tracedecay-rusqlite-runtime/src/graph` module, graph parity cases, and obsolete `graph.db` clone/branch logic. Do not leave an adapter facade.

- [ ] **Step 5: Verify and commit**

```bash
cargo nextest run -p tracedecay-graph-db -p tracedecay-code-index -p tracedecay-query --no-fail-fast
cargo clippy -p tracedecay-graph-db -p tracedecay-code-index -p tracedecay-query --all-targets --all-features -- -D warnings
git commit -am "refactor(code-graph): route Grafeo through graph-db"
```

## Task 4: Move semantic vectors and hybrid retrieval

**Files:**
- Modify: `crates/tracedecay-semantic/src/projector.rs`
- Modify: `crates/tracedecay-semantic/src/runtime_query.rs`
- Modify: `crates/tracedecay-usecases/src/store/vector_generations.rs`
- Modify: `src/store/vector_generations.rs`
- Modify: `crates/tracedecay-query/src/retrieval/semantic/execution_authority.rs`
- Modify: `crates/tracedecay-runtime-core/src/memory/store/vectors.rs`
- Delete: SQLite vector payload/state tables superseded by graph-db

**Interfaces:**
- Consumes: `EmbeddingProjectionKeyV1`, verified `EmbeddingVectorV1`, source generation, model/artifact digest, metric, dimensions, and normalization.
- Produces: atomic vector-generation publication and bounded exact Grafeo similarity/hybrid search with deterministic score normalization.

```rust
pub trait SemanticVectorStore {
    fn publish_generation(
        &self,
        generation: &VectorGenerationIdV1,
        vectors: Vec<AdmittedSemanticVector>,
        cancellation: &CancellationToken,
    ) -> Result<GraphWatermark, SemanticStoreError>;

    fn search(
        &self,
        request: &SemanticVectorSearchRequest,
    ) -> Result<SemanticVectorSearchPage, SemanticStoreError>;
}
```

- [ ] **Step 1: Add failing vector behavior tests**

Cover cosine/dot/euclidean behavior as declared by the manifest, dimension mismatch, non-finite values, stale generation, cancelled query, filtered namespace, deterministic ties, reopen, and serving the previous generation during rebuild.

- [ ] **Step 2: Move vector payloads and indexes to graph-db**

Store the vector on the canonical entity or a typed vector entity linked to it. Keep model lifecycle/install metadata and publication receipts relational. Publish vector and code graph generations in one graph-db batch when they share a generation.

- [ ] **Step 3: Implement hybrid retrieval above the storage primitive**

Grafeo returns bounded vector/BM25 candidates. `tracedecay-query` retains authorization, exact/lexical/graph fusion, score-domain normalization, coverage, hydration, explanations, and stable cursors.

- [ ] **Step 4: Delete SQLite vector payload duplication**

Remove `semantic_vector_payload_v1`, `semantic_vector_state_slice_v1`, evaluation duplicates, and legacy vector blobs after every production reader moves. Retain only relational generation/model lifecycle records that are not duplicate vector authority.

- [ ] **Step 5: Verify and commit**

```bash
cargo nextest run -p tracedecay-semantic -p tracedecay-query -p tracedecay-usecases --no-fail-fast
cargo bench --bench code_search --no-run
git commit -am "refactor(semantic): move vector search to graph-db"
```

## Task 5: Move Git topology and evidence relations

**Files:**
- Modify: `src/graph/git.rs`
- Modify: `crates/tracedecay-code-index/src/git_join.rs`
- Modify: `crates/tracedecay-usecases/src/git_reads.rs`
- Modify: `crates/tracedecay-usecases/src/git_query.rs`
- Modify: `crates/tracedecay-usecases/src/git_intelligence.rs`
- Modify: `crates/tracedecay-sessions/src/runtime/git_correlation.rs`
- Modify: `crates/tracedecay-domain/src/git.rs`
- Modify: `crates/tracedecay-domain/src/research/git_topology.rs`

**Interfaces:**
- Consumes: repository identity plus `gix`-derived refs, commits, parents, trees, worktrees, branches, snapshots, and evidence anchors.
- Produces: atomic `GitTopologyProjection` and typed ancestor/descendant/merge-base/change-impact traversals.

```rust
pub trait GitTopologyStore {
    fn replace_topology(
        &self,
        projection: GitTopologyProjection,
        cancellation: &CancellationToken,
    ) -> Result<GraphWatermark, GitTopologyError>;

    fn traverse(
        &self,
        request: GitTopologyRequest,
    ) -> Result<GitTopologyPage, GitTopologyError>;
}
```

- [ ] **Step 1: Add failing topology tests**

Cover merge commits, octopus parents, rewritten refs, detached HEAD, deleted branch, linked worktree identity, shallow/missing objects, cancellation, and deterministic reachability.

- [ ] **Step 2: Project gix authority into Grafeo**

`gix` remains the Git object/ref authority. Grafeo is the sole persisted topology/evidence projection; store source object IDs and ref watermark so stale projections fail truthfully.

- [ ] **Step 3: Replace custom traversal and graph-shaped SQL**

Move parent/reachability and commit-to-code/session/work relations to graph-db. Keep Git index transaction commitments and effect receipts in SQLite.

- [ ] **Step 4: Verify and commit**

```bash
cargo nextest run -p tracedecay-code-index -p tracedecay-usecases -p tracedecay-sessions git --no-fail-fast
git commit -am "refactor(git): project topology into graph-db"
```

## Task 6: Move LCM and session relation DAGs

**Files:**
- Modify: `crates/tracedecay-sessions/src/runtime/lcm/dag.rs`
- Modify: `crates/tracedecay-sessions/src/runtime/lcm/query.rs`
- Modify: `crates/tracedecay-sessions/src/runtime/lcm/schema.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/schema.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/projection/materialize.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/projection/persist.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/projection/receipts.rs`
- Modify: `crates/tracedecay-temporal-query/src/ports.rs`
- Modify: `crates/tracedecay-temporal-query/src/resolution/resolver.rs`
- Modify: `crates/tracedecay-temporal-query/src/resolution/summary.rs`
- Modify: `tests/session_suite/lcm_dag.rs`

**Interfaces:**
- Consumes: canonical session/message/summary identity, owning-store content locator, source lineage, successor, logical-copy, thread, and agent relations.
- Produces: bounded summary/source expansion, ancestry, successor, thread, agent, and logical-copy traversal with exact owning-store hydration.

```rust
pub trait SessionRelationGraph {
    fn publish(
        &self,
        event: SessionRelationEvent,
    ) -> Result<GraphWatermark, SessionRelationError>;

    fn select(
        &self,
        request: SessionRelationSelection,
    ) -> Result<SessionRelationPage, SessionRelationError>;
}
```

- [ ] **Step 1: Add failing lineage and recovery tests**

Cover cycles rejected at write, shared summary sources, missing/redacted payloads, cross-project denial, stale projection, cancellation, pagination, reopened persistence, and exact owning-store hydration.

- [ ] **Step 2: Move relation tables to graph-db**

Move only relation structure. Keep raw messages, summaries' authoritative content/payload refs, redaction, retention journals, replay transactions, and GC state in SQLite.

- [ ] **Step 3: Delete duplicate temporal relation tables**

Remove `session_summary_sources`, `session_summary_successors`, `session_logical_copy_edges`, `session_thread_hierarchy_edges`, and `session_agent_hierarchy_edges` after every query and Doctor caller uses graph-db. Keep no compatibility views.

- [ ] **Step 4: Verify and commit**

```bash
cargo nextest run -p tracedecay-sessions -p tracedecay-global-db -p tracedecay-temporal-query lcm --no-fail-fast
git commit -am "refactor(sessions): move relation DAGs to graph-db"
```

## Task 7: Move V2 memory graph and holographic vectors

**Files:**
- Modify: `crates/tracedecay-runtime-core/src/db/memory_v2/mod.rs`
- Modify: `crates/tracedecay-runtime-core/src/db/memory_v2/types.rs`
- Modify: `crates/tracedecay-runtime-core/src/db/memory_v2/schema/baseline.rs`
- Delete: `crates/tracedecay-runtime-core/src/db/memory_v2/cutover.rs`
- Delete: `crates/tracedecay-runtime-core/src/db/memory_v2/schema/compatibility.rs`
- Delete: `crates/tracedecay-runtime-core/src/db/memory_v2/schema/upgrades.rs`
- Modify: `crates/tracedecay-runtime-core/src/db/memory_v2/writers/lineage.rs`
- Modify: `crates/tracedecay-runtime-core/src/db/memory_v2/writers/purge.rs`
- Modify: `crates/tracedecay-runtime-core/src/memory/store/vectors.rs`
- Modify: `crates/tracedecay-application/src/memory.rs`
- Modify: `crates/tracedecay-dashboard-api/src/memory_service/graph.rs`
- Modify: `crates/tracedecay-dashboard-api/src/memory_analysis.rs`
- Delete: `crates/tracedecay-semantic/src/legacy_migration.rs`
- Delete: `crates/tracedecay-migrate/src/memory_cutover.rs`
- Delete: `crates/tracedecay-migrate/src/consolidate/sqlite/memory_v2.rs`

**Interfaces:**
- Consumes: project-wide fact/assertion/entity identities, trust and feedback events, FHRR/HRR bank identity, semantic vectors, retention policy, and exact provenance.
- Produces: project-wide fact/entity/relation traversal, holographic binding/unbinding candidates, semantic similarity, curation, feedback, retention, and diagnostics.

```rust
pub trait MemoryGraphStore {
    fn publish(
        &self,
        event: ProjectMemoryGraphEvent,
    ) -> Result<GraphWatermark, MemoryStoreError>;

    fn search(
        &self,
        request: ProjectMemoryGraphRequest,
    ) -> Result<ProjectMemoryGraphPage, MemoryStoreError>;
}
```

- [ ] **Step 1: Add failing complete fact-store journey tests**

Exercise `tracedecay_fact_store` add/search/probe/related/reason/contradict/get/update/remove/list, trust feedback, entity traversal, FHRR/HRR retrieval, semantic ranking, restart, retention, and worktree deletion. Assert facts never vary by branch/worktree.

- [ ] **Step 2: Split relational fact content from graph/vector state**

Keep exact content, provenance, trust history, current-fact CAS, feedback, deletion tombstones, and retention receipts relational. Store entity/assertion/relation topology and holographic/semantic vectors only in graph-db, keyed by the same project-wide typed IDs.

- [ ] **Step 3: Delete legacy and branch-era memory machinery**

Remove `memory_facts`, V1/V2 dual-write/fallback, cutover/migration/consolidation code, branch-only fact fixtures, archive-merge receipts, and compatibility schemas. An unexpected old store returns `ResetRequired`.

- [ ] **Step 4: Verify and commit**

```bash
cargo nextest run -p tracedecay-runtime-core -p tracedecay-application -p tracedecay-dashboard-api memory --no-fail-fast
git commit -am "refactor(memory): move graph and vectors to graph-db"
```

## Task 8: Move Work dependency and current topology

**Files:**
- Modify: `crates/tracedecay-domain/src/work.rs`
- Modify: `crates/tracedecay-domain/src/work_read.rs`
- Modify: `crates/tracedecay-application/src/work.rs`
- Modify: `crates/tracedecay-application/src/work_read.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work/events.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work/projection.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work/schema.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work/sql.rs`
- Modify: `crates/tracedecay-dashboard-api/src/work_api.rs`

**Interfaces:**
- Consumes: immutable Work events, expected item/graph versions, typed gating and informational relations.
- Produces: rebuildable current topology, readiness, blockers, critical path, impact, history links, and TaskId-rooted evidence traversal.

```rust
pub trait WorkTopologyStore {
    fn project(
        &self,
        event: &WorkEventV1,
        expected: WorkGraphVersion,
    ) -> Result<WorkGraphVersion, WorkStoreError>;

    fn read(
        &self,
        request: WorkGraphReadRequest,
    ) -> Result<WorkGraphReadPage, WorkStoreError>;
}
```

- [ ] **Step 1: Add failing DAG and projection tests**

Cover cycle rejection for gating edges, allowed cycles for informational edges, stale version/CAS, deterministic topological order, critical path, fan-out, cancellation, rollback, and rebuild byte-identity.

- [ ] **Step 2: Project Work events into graph-db**

SQLite remains canonical for immutable event payloads, idempotency, receipts, attempts, leases, effects, and artifact metadata. Grafeo becomes sole current dependency/topology projection and is rebuildable from the event watermark.

- [ ] **Step 3: Delete graph-shaped Work projection SQL**

Remove topology blobs/deltas and SQL traversals that duplicate Grafeo. Retain only event/attempt/receipt tables and the exact projection watermark needed for recovery.

- [ ] **Step 4: Verify and commit**

```bash
cargo nextest run -p tracedecay-domain -p tracedecay-application -p tracedecay-rusqlite-runtime work --no-fail-fast
git commit -am "refactor(work): move task topology to graph-db"
```

## Task 9: Move workflow definition and run topology

**Files:**
- Modify: `crates/tracedecay-rusqlite-runtime/src/workflow.rs`
- Modify: `crates/tracedecay-application/src/workflow_catalog.rs`
- Modify: `crates/tracedecay-application/src/workflow_coordination.rs`
- Modify: `crates/tracedecay-application/src/workflow_runtime.rs`
- Modify: `crates/tracedecay-sessions/src/runtime/workflow_index.rs`
- Modify: `src/daemon/workflow_runtime.rs`

**Interfaces:**
- Consumes: typed workflow definitions, nodes, dependencies, activations, handoffs, executions, attempts, and receipts.
- Produces: validated workflow DAG, ready-node traversal, run/attempt topology, and bounded history/evidence reads.

```rust
pub trait WorkflowTopologyStore {
    fn publish_definition(
        &self,
        definition: &WorkflowDefinitionV1,
    ) -> Result<GraphWatermark, WorkflowStoreError>;

    fn read(
        &self,
        request: WorkflowTopologyRequest,
    ) -> Result<WorkflowTopologyPage, WorkflowStoreError>;
}
```

- [ ] **Step 1: Add failing workflow topology tests**

Cover definition-cycle rejection, missing node, stale definition, ready-node ordering, fan-out/join, cancellation, fenced attempt, restart recovery, and provider-independent execution.

- [ ] **Step 2: Move definition/run graph structure to graph-db**

Keep queues, leases, activation CAS, effects, idempotency, execution receipts, and runtime clocks in SQLite. Store definition edges and rebuildable run/attempt/handoff topology in Grafeo.

- [ ] **Step 3: Delete duplicate workflow graph tables**

Delete graph-shaped definition/handoff/run topology rows after all application/session callers use graph-db. Retain no parallel read path.

- [ ] **Step 4: Verify and commit**

```bash
cargo nextest run -p tracedecay-application -p tracedecay-sessions -p tracedecay-rusqlite-runtime workflow --no-fail-fast
git commit -am "refactor(workflow): move DAG topology to graph-db"
```

## Task 10: Wire cross-domain production journeys

**Files:**
- Modify: `crates/tracedecay-store/src/runtime/identity.rs`
- Modify: `crates/tracedecay-store/src/runtime/lifecycle.rs`
- Modify: `crates/tracedecay-store/src/runtime/operation.rs`
- Modify: `crates/tracedecay-store/src/runtime/ports.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/mod.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/profile_paths.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/registry.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/registry/attachment.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/registry/close.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/registry/leases.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/registry/open.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/registry/ports.rs`
- Modify: `crates/tracedecay-runtime-core/src/store_runtime/resolver.rs`
- Modify: `src/daemon/graph_resolution.rs`
- Modify: `src/tracedecay/graph_runtime_port.rs`
- Modify: `src/tracedecay/queries/graph.rs`
- Modify: `src/mcp/tools/handlers/graph.rs`
- Modify: `crates/tracedecay-dashboard-api/src/graph_service.rs`
- Modify: `crates/tracedecay-dashboard-api/src/project_graph.rs`

**Interfaces:**
- Consumes: exact project/profile/store identity, graph namespace, policy scope, source watermarks, cancellation, and request budget.
- Produces: one daemon-owned graph-db registry plus typed code/Git/session/memory/Work/workflow query adapters.

```rust
pub trait GraphRuntimeRegistry {
    fn resolve(
        &self,
        route: &ExactProjectRoute,
        cancellation: &CancellationToken,
    ) -> Result<Arc<GraphDb>, GraphRouteError>;

    fn close(
        &self,
        identity: &StoreIdentity,
        deadline: ShutdownDeadline,
    ) -> Result<(), GraphRouteError>;
}
```

- [ ] **Step 1: Add failing production routing tests**

Cover linked worktrees sharing project graph identity, multi-root routing, cross-project denial, missing registry, unavailable store, stale projection, shutdown cancellation, concurrent readers, serialized writer admission, and no fallback to the active graph.

- [ ] **Step 2: Register one graph-db runtime per exact project/profile authority**

The daemon registry owns open/close, writer serialization, snapshot leases, retention, and health. MCP, CLI, HTTP/dashboard, LSP, hooks, workers, and tests never open graph-db directly.

- [ ] **Step 3: Wire retained tools and views**

Exercise code graph, Git context, LCM, fact store, Work/workflow, semantic search, Doctor, storage telemetry, and dashboard graph views through the same application ports.

- [ ] **Step 4: Verify and commit**

```bash
cargo nextest run --workspace --all-features --no-fail-fast
git commit -am "feat(graph-db): wire embedded graph journeys"
```

## Task 11: Delete superseded SQL, migrations, parity, and sidecar residue

**Files:**
- Delete/modify: `crates/tracedecay-runtime-core/src/db/migrations.rs`
- Delete/modify: `crates/tracedecay-rusqlite-parity/src/fixture_ddl.rs`
- Modify: `crates/tracedecay-sqlite-parity-protocol/src/request.rs`
- Modify: `crates/tracedecay-sqlite-parity-protocol/src/response.rs`
- Modify: `crates/tracedecay-sqlite-parity-protocol/src/results.rs`
- Modify: `crates/tracedecay-sqlite-parity-protocol/src/tests.rs`
- Delete: `crates/tracedecay-migrate/src/memory_cutover.rs`
- Delete: `crates/tracedecay-migrate/src/consolidate/mod.rs`
- Delete: `crates/tracedecay-migrate/src/consolidate/sqlite.rs`
- Modify: `crates/tracedecay-migrate/src/lib.rs`
- Modify: `docs/plans/tracedecay-v2/*.md`
- Modify: `docs/DESIGN-DOC.md`
- Modify: `docs/dashboard.md`

**Interfaces:**
- Consumes: completed production cutovers from Tasks 3–10.
- Produces: one final V2 data design with no dead compatibility or duplicate graph authority.

- [ ] **Step 1: Prove every old caller moved**

Run:

```bash
rg -n 'CREATE TABLE.*(nodes|edges|vectors)|WITH RECURSIVE|memory_facts|branch_only_fact|petgraph|graph sidecar|graph migration' src crates docs scripts tests
```

Classify every hit. Retained hits must be relational fixtures/docs that describe the final design; all superseded production hits are deleted.

- [ ] **Step 2: Delete complete obsolete boundaries**

Delete SQLite graph/vector fixtures and protocols, graph branch cloning, V1/V2 graph and memory migrations, backfills, compatibility readers, feature flags, aliases, and unused dependencies. Remove a dependency with its last production caller.

- [ ] **Step 3: Verify documentation authority**

Update the V2 plan set to name Grafeo and `tracedecay-graph-db`; remove petgraph, sidecar, branch-fact, dual-write, old graph-SQL, and migration language from active plans.

- [ ] **Step 4: Verify and commit**

```bash
cargo machete
cargo check --workspace --all-features
git diff --check
git commit -am "refactor(storage): delete superseded graph SQL"
```

## Task 12: Performance, durability, and CI acceptance

**Files:**
- Create: `crates/tracedecay-graph-db/benches/code_traversal.rs`
- Create: `crates/tracedecay-graph-db/benches/vector_search.rs`
- Create: `crates/tracedecay-graph-db/benches/mixed_read_write.rs`
- Modify: `scripts/tool-sweep.sh`
- Modify: `.github/workflows/ci.yml`
- Modify: `docs/plans/tracedecay-v2/33-end-to-end-performance-optimization.md`
- Modify: `docs/plans/tracedecay-v2/NEXT.md`

**Interfaces:**
- Consumes: final graph-db-backed production journeys.
- Produces: falsifiable latency, memory, durability, and cross-platform evidence.

- [ ] **Step 1: Add representative benchmarks**

Measure cold/open and warm traversal, 1/2/4-hop bounded code graphs, Git ancestry, LCM expansion, Work readiness, 100k/1m vector search, concurrent readers plus one writer, generation replacement, reopen, and retained-store size.

- [ ] **Step 2: Add failure and crash recovery tests**

Cover interrupted projection replacement, corrupt persistent store, partial vector batch, writer cancellation, reader snapshot during publication, daemon shutdown, foreign final-shape rejection, and explicit fresh-profile recreation.

- [ ] **Step 3: Run the full product gate in an isolated profile**

```bash
cargo nextest run --workspace --all-features --no-fail-fast
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
(cd dashboard && npm run contracts:check && npm run typecheck && npm test && npm run build)
bash scripts/tool-sweep.sh
```

Expected: all tests execute non-vacuously; any inherited failure remains named with owning lane and exact evidence. Graph-tool deadlines are not raised to hide regressions.

- [ ] **Step 4: Compare against the pre-Grafeo baseline**

Record p50/p95/p99 latency, peak RSS, store bytes, write amplification, and reopen time for the same fixtures. Reject the cutover if a retained production journey regresses materially without a documented product reason.

- [ ] **Step 5: Final parent review, merge, push, and worktree cleanup**

Review every task commit and full branch diff against the current integration floor. Merge only coherent passing lanes. Remove only recognized team-owned worktrees after their commits are merged and reachable from `codex/tracedecay-total-redesign-plan`.
