# PR9 Contract Spine (pr9/00-contract-spine)

Status: historical architecture record. Current product requirements live in
the linked owning plans and direct tests. This file is not an acceptance
authority, packet index, gate, append-only record, or instruction to recreate
the former PR9 decomposition.

Canonical sources: [Plan 25](../25-code-intelligence-indexing-crate.md),
[Plan 05](../05-query-crate.md),
[Plan 15](../15-search-quality-evaluation-and-retrieval-research.md),
[Plan 36](../36-git-aware-change-context-and-index-transactions.md)
(read-only Git semantics only),
[Plan 35](../35-daemon-lsp-gateway-and-universal-diagnostics.md)
(generation-bound diagnostic contracts only), and the
[Plan 19 charter](../19-system-defragmentation-convergence-and-extensibility.md).

## 1. Retained architecture decisions

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
   versioned exact+lexical+graph result — IDs, order, contributions,
   explanations, coverage, and cursor bytes — canonical-encoded and hashed
   independently with the schema/domain separator
   `tracedecay.pr9-fallback.v1`; the digest field itself is excluded from
   the hashed bytes. Its lane-coverage map may contain only `ExactLiteral`,
   `Lexical`, and `Graph` (enforced by `validate()` and contract tests).
   PR10 may report semantic/rerank status only outside the subpayload; it
   cannot change its candidates, PR9 contributions, explanations, coverage,
   cursor, digest, or cache identity. Private `internal_lane_outcomes` is
   excluded from fallback bytes/digest, cursors, public coverage, and cache
   keys; public statuses coalesce denied and absent evidence.
6. **Generation-bound diagnostics.** Plan 35 owns
   `GenerationDiagnosticV1` in `crates/tracedecay-domain/src/diagnostics.rs`
   and its direct regressions. The code index stores only typed anchor
   references (`GenerationDiagnosticAttachmentV1`) — never a
   duplicate diagnostic record. Dirty LSP overlays cannot enter clean
   generations; stale findings cannot cross snapshots.
7. **Read-only Git.** Plan 36 owns native read-only status,
   working/staged/range diff, history, blame, rename, binary, merge, and
   `HunkRef` semantics. PR9 adapters join
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
| Generation-bound diagnostic record | Plan 35 | Canonical diagnostic domain/application owner |
| Read-only Git semantics, `HunkRef` | Plan 36 | Native Git owner |
| Developer evaluation fixtures, corpus, labels, profile selection | Plan 15 | Direct tests and Linux developer eval |
| Active/rollback profile pointer CAS | Plan 20 | Configuration runtime |
| Artifact/vector storage | Plan 02 | Store runtime |
| Projection/checkpoint/publication semantics | Plan 04 | PR9 consumes `ProjectionBatchRequestV1`/`CodeChunkProjectionReceiptV1` contracts only |
| Daemon/service scheduling | daemon orchestration | not this spine |

## 3. Historical delivery decomposition

The former `pr9/10-13` packet names divided extraction/chunking, read-only Git,
generation diagnostics, and search evaluation. They are historical labels
only. Current work follows the owning plans and direct tests; no packet,
authority checkpoint, aggregate-acceptance branch, holdout metadata, run
schema, or evidence schema is recreated.

## 4. Query-crate extraction decision

**Decision: retain root modules for now.** `src/query/retrieval/` and
`src/code_index/` remain root modules that honor the crate ownership contract
(typed domain requests, read-only port traits, no SQL/transport/policy imports)
and the existing architecture-boundary tests.

A future extraction requires a real second consumer, a simple same-host Linux
Cargo clean-build/warm-incremental comparison with raw timings, dependency
isolation, unchanged public contracts, and normal all-feature CI. If those direct checks do not show a
practical benefit, retain the root modules. Do not create an extraction packet,
gate manifest, committed decision record, or clean/content-addressed checkout
snapshot.

## 5. Conformance notes

- `crates/tracedecay-domain` remains pure values and validation: no I/O,
  persistence, query execution, policy evaluation, host integration, or
  async work; dependencies stay `serde`, `serde_json`, `sha2`, `thiserror`.
- `src/query/**` conforms to the query-kernel source guard (allowlisted
  import roots, derives, attributes, and macros; single module root at
  `src/query/mod.rs`; conventional module files). Contract traits carry no
  bodies; lane/stage logic remains with the owning production modules.
- `src/code_index/` keeps capture as the only intake and store/projector
  composition as the only publication path (Plan 25 acceptance).
- Architecture-test edits required for this spine: **none**. All new modules
  pass the existing guards as structured; no allowlist was modified.
