# 40 — Partitioned generation storage and zero-copy replay retirement

Status: ACTIVE (decided 2026-08-22). Supersedes the monolithic sealed-generation
envelope as the long-term storage design; stages 0–1 land first as compatible
fixes inside the current `format_revision`.

Stage state (reconciled 2026-08-31 against the tree, not the commit log):
stages 0a, 0b, and 0c are landed; stages 1 and 2 are landed; stage 3 is
untouched. Partitioned writers emit `format_revision` 7 while readers retain
explicit revision-5/6 monolithic compatibility. The
§Problem measurements below describe the pre-stage-0 state and are retained
as the original evidence, not as a description of the current tree.

## Problem (measured, 2026-08-21/22, this repository)

- One sealed code generation for this repository is a single
  `generation-<digest>.json` of **1.39 GB**, built fully in memory by
  `serde_json::to_vec` (`sealed_codec.rs`) before any I/O.
- Cold activation parses that JSON **twice** — once for query serving
  (`decode_active_generation`) and once for graph projection
  (`hydrate_sealed_code_generation`) — from **two byte-identical files**,
  because graph publication eagerly stages a private copy into
  `<db>.graph-replay/` (`seals.rs::stage_project_graph_replay_seal`) as
  crash-recovery insurance against the retention sweep unlinking the source.
- Byte attribution (sampled): actual chunk text ≈ 220 MB; the majority is
  repeated long string identities (`chunk_id` ≈ 76 MB, `symbol_occurrence_id`
  ≈ 60 MB, `generation_id` repeated ≈ 42 MB) plus per-record digest chains and
  JSON structure.
- Grafeo (`tracedecay.grafeo`) holds only compact symbol/entity records;
  query serving never reads it. Every daemon start rebuilds all serving
  structures in RAM from the JSON. Warmup on this repository read tens of GB,
  pinned the 8 GiB cgroup boundary, and starved queries.

Master (pre-V2) proved the opposite trade-offs work — SQLite adjacency graph,
per-file incremental sync, no stored body text, open-and-serve startup — but
lacked what V2 genuinely adds: immutable digest-sealed generations,
verify-then-publish with a watermark, exact branch/revision identity.
Doc 39 already assigns the durable graph to Grafeo; the cutover never
completed, leaving the sealed JSON as both canonical authority **and** de
facto serving store.

## Decision

Keep V2's invariants (sealed immutable generations, content addressing,
verify-then-publish, bounded retention, branch identity). Change the physical
representation and the replay economics:

1. **Tiny atomic generation manifest** — identity, format revision, git
   evidence, and the digest + location of every component. The manifest is the
   only whole-generation JSON.
2. **Partitioned canonical segments** — per-file (or bucketed) compact
   extraction segments, content-addressed so unchanged files' segments are
   shared across generations. Chunk text, exact terms, subtokens, symbols live
   here, not in one envelope. (The bounded lexical artifact layer on
   `codex/text-graph-degradation` is the lexical partition of this scheme.)
3. **Grafeo durably owns the graph** under the generation ID (staging
   generation during indexing, committed before manifest publish). Startup
   validates manifest ↔ Grafeo verified head and serves with **no replay**.
4. **Replay is lazy, scoped, and background.** Missing/corrupt state enqueues
   partitioned replay for the requested scope; queries return typed
   graph-pending coverage plus lexical/memory results meanwhile (degradation
   contract landed in #601).
5. **No second whole-generation copy, ever.** Replay reads canonical
   artifacts; retention must never require a byte-duplicating insurance copy.

## Staged delivery (each stage ships alone, oldest-first compatible)

- **Stage 0 — zero-copy replay retirement** (this change set, three commits):
  - **0a** `hydrate_sealed_code_generation` resolves the sealed payload from
    the canonical `code-generations-v1/` root first and falls back to the
    replay pool. Readers become location-agnostic; digest verification on read
    makes both locations equally trustworthy. Retirement moves strictly
    canonical→pool, so a canonical-then-pool probe cannot miss a live file.
  - **0b** Retention retires a superseded generation by **atomic rename into
    the replay pool** (no-clobber; content-identical `AlreadyExists` collapses
    to unlink) instead of unlinking, whenever the graph projection has not yet
    durably consumed it. Rename is metadata-only and crash-atomic: the file is
    always in exactly one of the two roots. The existing release queue keeps
    deleting pool entries once the graph append is durable.
  - **0c** Delete the eager staging copy from the graph publish path
    (`install_project_graph_replay_seal_at` / `stage_project_graph_replay_seal`
    and the copy machinery in `seals.rs`). Steady state stages **zero bytes**;
    the pool only ever holds generations that were retired while still needed.
- **Stage 1 — single-parse activation**: graph publication consumes the
  already-decoded active generation (the projection manifest already rides
  along on first publication; extend the same guarantee to the recovery
  branches via the Stage-0a fallback) so cold activation parses the sealed
  payload at most once.
- **Stage 2 — manifest + segment split** (`format_revision` bump): split the
  sealed envelope per §Decision 1–2; integer-keyed, content-addressed
  segments; the 2 GiB whole-envelope bound and whole-file rewrite per
  generation disappear; unchanged-file segments are shared across
  generations.
- **Stage 3 — Grafeo startup authority**: manifest ↔ Grafeo head validation
  replaces replay on clean startup per §Decision 3–4.

## Invariant mapping (old mechanism → new mechanism)

| Invariant | Today | After |
|---|---|---|
| Content addressing | digest-named monolithic JSON | digest-named manifest + digest-named segments |
| Atomic publish | temp+rename of envelope + pointer | segments/Grafeo commit first, then manifest temp+rename |
| Crash recovery | eager 1.39 GB pool copy + journal replay | rename-retired canonical files + scoped lazy replay |
| Retention | unlink source; release pool copy via queue | rename-to-pool when graph pending; queue unchanged |
| Verification | full-file digest on every decode | per-component digest on read (unchanged per file) |
| Branch identity | durable index entry evidence | unchanged (manifest carries it) |

## Non-goals

- No change to sealing semantics, projector revisions, or the relational
  journal contract in Stage 0.
- Stage 0 does not change `format_revision`; on-disk envelopes stay readable
  both directions across the stage-0 commits.

## Dated amendment (2026-09-12, recorded decision): Stage 2b — shared, compressed segments

**Measured.** For one worktree of a 150 MB / 5,190-file repository the scoped
store is 4.6 GB: a 2.76 GB text artifact whose term and ngram posting lists are
stored twice (document-clustered table plus term-leading covering index),
1.4 GB of per-file segment JSON (8x the source: chunk `sanitized_text` across
overlapping grains is 1.43x the source and `subtokens` another ~0.6x), and two
300 MB interactive read bundles. Each linked worktree — including the internal
`branch-worktrees` the daemon creates for branch tracking — repeats the whole
stack, and no bytes are shared: 52,463 segment files across nine worktree
scopes had 52,435 distinct digests because every segment embeds
`authority.worktree_id`.

**Decision.** Stage 2 keeps its manifest + segment split; Stage 2b changes
what a segment is and where it lives, under one `format_revision` bump:

1. **Worktree identity leaves the segment.** `authority.worktree_id` and the
   branch reference move to the generation manifest's file entry. A segment is
   then a pure function of file content, sanitizer, chunker, and descriptor
   revisions, so identical files hash identically across worktrees.
2. **One content store per repository.** Segments and text artifacts live
   under `code-index-v1/<repository_id>/`; each worktree scope keeps only its
   generation manifests, pointer, and freshness witness. `FileSegmentPlanV1::
   Reused` looks up the repository store, so a second checkout of the same
   commit publishes a manifest and no segment bytes. Retention's mark phase
   unions every worktree manifest of the repository plus the replay pool
   before sweeping the shared store; the scope-root record (`scope-root.v1`)
   already identifies each worktree's root.
3. **Block-compressed segments with a range index.** `SealedGenerationSegment
   ReadV1::Range` reads are served today by seeking into the JSON; a compressed
   segment carries a block table (uncompressed offset → compressed offset) so
   a range read decompresses one block. `flate2` is already in the workspace;
   a new dependency needs a measured ratio win over it.
4. **Text once per file.** A segment stores the sanitized file text once and
   each chunk carries `source_span` into it; `subtokens` are derived when the
   text artifact is built, not persisted.
5. **Text artifact V15.** Stage the posting lists document-clustered as today,
   then write one term-clustered table at finalization instead of a covering
   index that duplicates every row. Accept only against the V13/V14 append and
   probe measurements recorded in `builder.rs`.

**Order.** 1 and 2 ship together (they change the same digest); 3 and 4 can
follow within the same revision; 5 is independent. Each step carries the
dedup or byte figure it changes, measured on the repository above.
