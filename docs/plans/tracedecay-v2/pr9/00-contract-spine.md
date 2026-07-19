# PR9 Contract Spine (pr9/00-contract-spine)

Status: frozen contract checkpoint, delivered from `PR9_BASE` on
`codex/tracedecay-total-redesign-plan`. This document records the PR9 frozen
architecture decisions, the ownership map, the packet boundaries for the
pr9/10-13 authority packets, and the measured query-crate extraction
decision gate. It changes only through a new append-only revision with Sol
approval.

Canonical sources: [Plan 25](../25-code-intelligence-indexing-crate.md),
[Plan 05](../05-query-crate.md),
[Plan 15](../15-search-quality-evaluation-and-retrieval-research.md),
[Plan 36](../36-git-aware-change-context-and-index-transactions.md)
(read-only Git semantics only),
[Plan 35](../35-daemon-lsp-gateway-and-universal-diagnostics.md)
(generation-bound diagnostic contracts only), and the
[Plan 19 charter](../19-system-defragmentation-convergence-and-extensibility.md).

## 1. Frozen decisions

1. **Single-root PR9 scope.** PR9 owns one complete single-root
   exact/lexical/graph vertical. "Federation" in Plan 15 means composing
   independent evidence lanes inside one authorized root. Plan 16 multi-root
   scope-set resolution, per-shard continuations, and cross-root rank
   fallback remain PR15 work. No PR9 type carries shard or root-set
   identity.
2. **One generic retrieval kernel.** The common `Retriever<R, E>`,
   `RetrieverBatch<E>`, `CompactCandidate`, fixed-point fusion, diversity,
   hydration, cursor, explanation, and coverage contracts live in
   `crates/tracedecay-domain/src/retrieval.rs` (pure values and trait
   surfaces) and `src/query/retrieval/` (root-side port composition). Code
   chunks/documents adapt into `CompactCandidate`. No code-specific
   `RankedChannelList`, fusion-profile, contribution, candidate, cursor, or
   hydration type hierarchy may appear under `code_intelligence/`. PR10 does
   not create a second lexical/fusion hierarchy.
3. **True independent exact lane.** `src/query/retrieval/exact.rs` consumes
   only whole exact technical terms plus the centrally minted
   `ExactAdmissionProof`; it is independent of the fielded lexical/BM25 lane
   (`lexical.rs`), which consumes whole-term and language-profiled subtoken
   postings independently. Exact and lexical are independently disableable.
   Only the central `ExactAdmissionValidator` mints proofs; retrievers
   cannot assign an exact tier. An approximate, graph-only, or later
   semantic candidate cannot precede an eligible exact result.
4. **Semantic/task lanes report `unavailable`.** Semantic (PR10), temporal
   export, task/session (PR17), and diagnostic lanes are capability-reported
   through the typed `CapabilityReportedLane` /
   `RetrieverOutcome::Unavailable` contract
   (`src/query/retrieval/unavailable.rs`). Missing authority is never
   simulated or replaced with a heuristic lookalike.
5. **Typed fallback subpayload.** `Pr9FallbackSubpayload` is the canonical
   accepted exact+lexical+graph result — IDs, order, contributions,
   explanations, coverage, and cursor bytes — canonical-encoded and hashed
   independently with the schema/domain separator
   `tracedecay.pr9-fallback.v1`; the digest field itself is excluded from
   the hashed bytes. Its lane-coverage map may contain only `ExactLiteral`,
   `Lexical`, and `Graph` (enforced by `validate()` and contract tests).
   PR10 may report semantic/rerank status only outside the subpayload; it
   cannot change its candidates, PR9 contributions, explanations, coverage,
   cursor, digest, or cache identity. Sealed `internal_lane_outcomes` is
   excluded from fallback bytes/digest, cursors, public coverage, and cache
   keys; public statuses coalesce denied and absent evidence.
6. **Generation-bound diagnostics.** Plan 35 owns
   `GenerationDiagnosticV1` in `crates/tracedecay-domain/src/diagnostics.rs`
   (pr9/12-diagnostic-persistence packet). The code index stores only typed
   anchor references (`GenerationDiagnosticAttachmentV1`) — never a
   duplicate diagnostic record. Dirty LSP overlays cannot enter clean
   generations; stale findings cannot cross snapshots.
7. **Read-only Git.** Plan 36 owns native read-only status,
   working/staged/range diff, history, blame, rename, binary, merge, and
   `HunkRef` semantics (pr9/11-readonly-git-core packet). PR9 adapters join
   typed Git results to generation-matched symbols, callers, hazards,
   diagnostics, and affected-test candidates; they never reconstruct Git
   objects, patches, history, or blame from indexed rows, and they never
   mutate the repository.
8. **Storage-neutral code intelligence.** PR9 code-intelligence values live
   in `crates/tracedecay-domain/src/code_intelligence/` (identity,
   language, search, index) and are immutable logical records — no store
   rows, no runtime, no transport, no parser acquisition. The initial
   implementation lives in root modules under `src/code_index/`.
9. **Rank before hydrate.** Retrieval, fusion, dedupe, and diversity
   operate on compact candidates; final context hydration occurs only for
   the selected result set, after a repeated authorization check, under
   byte/token/deadline budgets, with one `HydrationReceipt` per anchor.

## 2. Ownership map

| Concern | Owner | PR9 home |
| --- | --- | --- |
| Retrieval kernel values/traits, `Pr9FallbackSubpayload` | Plan 15 | `crates/tracedecay-domain/src/retrieval.rs` |
| Query port composition, lane adapters, stages | Plan 05 (+ Plan 15 contracts) | `src/query/retrieval/{mod,ports,exact,lexical,graph,fusion,dedupe,diversity,hydrate,unavailable}.rs` |
| Code-intelligence values (descriptors, chunks, generations, lineage, test attribution) | Plan 25 | `crates/tracedecay-domain/src/code_intelligence/{mod,identity,language,search,index}.rs` |
| Code-index implementation spine | Plan 25 | `src/code_index/{mod,intake,languages,extract,chunks,capabilities}.rs` |
| Generation-bound diagnostic record | Plan 35 | `crates/tracedecay-domain/src/diagnostics.rs` (pr9/12 packet; **not** part of this spine) |
| Read-only Git semantics, `HunkRef` | Plan 36 | pr9/11 packet (**not** part of this spine) |
| Evaluation fixtures, corpus, labels, promotion policy | Plan 15 | pr9/13 packet (`benchmarks/search-quality/`) |
| Trust roots, signed profiles, active/rollback pointer CAS | Plan 20 | PR10 packets |
| Artifact/vector storage | Plan 02 | PR10 packets |
| Projection/checkpoint/publication semantics | Plan 04 | PR10 packets; PR9 consumes `ProjectionBatchRequestV1`/`CodeChunkProjectionReceiptV1` contracts only |
| Daemon/service scheduling | daemon orchestration | not this spine |

## 3. Packet boundaries (pr9/10-13 authority packets)

- **pr9/10-registry-extraction-chunks** implements the `src/code_index/`
  spine: `CodeIndexIntake::validate`, `LanguageRegistry`,
  `LanguageExtractor::extract`, `CodeChunker::chunk_file`, and
  `CodeIndexCapabilityEmitter::emit`, plus isolated tests. It consumes the
  domain values exactly as declared; it may not rename, re-field, or
  re-crate them.
- **pr9/11-readonly-git-core** adds Plan 36 domain Git types, the native
  adapter, and differential fixtures. It must not add Git object storage,
  revision traversal authority, patch reconstruction, or any mutation path
  to the code-index or query modules.
- **pr9/12-diagnostic-persistence** adds
  `crates/tracedecay-domain/src/diagnostics.rs` (`GenerationDiagnosticV1`),
  store/application ports, and restart/clear/supersession/dirty-overlay
  tests. The code index joins diagnostics only through
  `GenerationDiagnosticAttachmentV1` anchor references.
- **pr9/13-search-eval-fixtures** freezes the real sanitized corpus,
  contamination partitions, sealed holdout metadata, run/evidence schemas,
  exact-admission oracles, and authorization canaries under
  `benchmarks/search-quality/`. Synthetic fixtures may prove contracts but
  cannot stand in for locked quality or resource evidence.

Sol composes `pr9/i1-authority-checkpoint` from those four packets, then
`pr9/i2-generations` (generations/lineage/projection receipts/joins),
`pr9/i3-query-ready` (V1 migration + exact/lexical/graph adapters + fusion
stages), and `pr9/49-aggregate-acceptance`.

## 4. Measured query-crate extraction decision gate

**Decision (recorded at this spine): retain root modules for now.**
`tracedecay-query` extraction is *conditional on paired compile evidence*;
`src/query/retrieval/` and `src/code_index/` land as root modules that honor
the crate ownership contract in place (typed domain requests, read-only port
traits, no SQL/transport/policy imports — enforced by the
`tests/architecture_boundaries` query-kernel guard). The identical rule
holds for `tracedecay-code-index`: it starts as root modules and extracts
only on the same evidence. Either outcome changes location only, never
contracts (Plan 05, "PR9 — extraction gate"; Plan 19, "Modules first").

A later packet (pr9/41-benchmarks-architecture or a dedicated
`pr9/00e-query-extraction-gate` replay) may flip this decision only by
producing **all** of the following evidence, in one committed record under
`benchmarks/pr9-code-index/extraction-gate-v1.json`:

1. **Named reuse.** The exact list of consumers outside the root package
   that import the shared query execution primitives (PR8 temporal kernel
   plus the PR9 lexical/code slice, by module path and symbol), proving a
   real second consumer exists.
2. **Paired compile measurement.** Same-host, same-toolchain, same-feature
   clean and warm-incremental `cargo check` wall/CPU times for the
   frequently touched compile graph **before and after** extraction, each
   run at least 5 untimed warmups plus 30 measured repetitions with all raw
   samples retained, showing a smaller frequently touched graph after
   accounting for added crate metadata, code generation, and linking
   (Plan 19 §3).
3. **Dependency isolation proof.** The post-extraction crate dependency
   graphs (`cargo metadata` digests) showing heavy grammars/model runtimes
   remain outside unrelated focused checks, and the updated
   architecture-boundary allowlists.
4. **Contract invariance.** A diff-free statement that every domain value
   and port trait named in §1-§2 is byte-identical after the move (location
   changes, contracts do not), with the aggregate all-feature check/test
   gate green on the extracted layout.

If any item is missing or the paired measurement does not show a smaller
frequently touched graph, the gate fails and root modules are retained
without changing contracts.

## 5. Conformance notes

- `crates/tracedecay-domain` remains pure values and validation: no I/O,
  persistence, query execution, policy evaluation, host integration, or
  async work; dependencies stay `serde`, `serde_json`, `sha2`, `thiserror`.
- `src/query/**` conforms to the query-kernel source guard (allowlisted
  import roots, derives, attributes, and macros; single module root at
  `src/query/mod.rs`; conventional module files). Contract traits carry no
  bodies; lane/stage logic lands in the authority/behavior packets.
- `src/code_index/` keeps capture as the only intake and store/projector
  composition as the only publication path (Plan 25 acceptance).
- Architecture-test edits required for this spine: **none**. All new modules
  pass the existing guards as structured; no allowlist was modified.
