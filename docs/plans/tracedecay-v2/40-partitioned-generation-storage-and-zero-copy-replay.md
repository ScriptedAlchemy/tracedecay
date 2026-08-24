# 40 — Partitioned generation storage and zero-copy replay retirement

Status: ACTIVE (decided 2026-08-22). Supersedes the monolithic sealed-generation
envelope as the long-term storage design; stages 0–1 land first as compatible
fixes inside the current `format_revision`.

Stage state (reconciled 2026-08-24 against the tree, not the commit log):
stages 0a, 0b, and 0c are landed; stage 1 is landed; stages 2 and 3 are
untouched. `format_revision` is still 6
(`SEALED_GENERATION_FORMAT_REVISION_V1`), as stages 0–1 require. The
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
