# PR10: Native FastEmbed semantic code search

**Status:** implementation authority for PR10. PR10 delivers configurable native semantic code search end to end; it is not a future experiment bucket.

## Outcome

TraceDecay augments exact lexical and graph search with local code embeddings.
Exact results remain authoritative without a model; similarity alone never proves impact, lineage, or equivalence.

## Ownership

- Plan 25 builds deterministic `SemanticCodeDocumentV1` and
  `SemanticCodeChunkV1` values from an exact code snapshot.
- Plan 04 schedules only changed eligible documents and resumable generation
  work.
- Plan 02 stores immutable vector generations, manifests, checkpoints, and the
  atomic active-generation pointer through daemon-owned writer authority.
- Plan 05 owns retrieval, deterministic fusion, explanations, and redundancy
  classification.
- Plan 09 owns model artifacts, runtime sessions, authorization, budgets,
  activation, rebuild, and status.
- Plans 10/11/20/21 expose the same application operations through API,
  dashboard, configuration, CLI, and MCP.

Only one root-private adapter depends on `fastembed`. Crates for indexing, store,
query, API, and UI depend on ports and stable domain values, never
FastEmbed runtime types.

## Deterministic documents and generations

Each chunk records repository/project/worktree/ref/snapshot identity, immutable
code generation, file and symbol identity, source span, language/extractor and
chunker versions, sensitivity decision, content digest, stable ordinal, and
bounded sanitized text. Symbol boundaries are preferred; oversized symbols use
versioned structural splits. Generated, vendor, binary, ignored, fixture, and
unsupported content has an explicit classification.

A vector-generation identity includes the ordered eligible-document manifest,
model/tokenizer/runtime manifest, dimension, metric, normalization, chunker,
privacy domain/key epoch, and source watermark. Builds checkpoint in bounded
batches. Partial or mixed generations are never queryable. Publication verifies
membership, dimensions, finite values, digests, and watermark before one atomic
pointer swap; deletion creates a tombstone and unchanged inputs do no embedding
work.

## Model and offline lifecycle

Configuration selects an installed signed embedding profile and, independently,
an optional reranker profile. Manifests pin actual model/tokenizer/config bytes,
licenses, runtime/build identity, dimensions, normalization, metric, device,
threads, batching, and resource ceilings. Implementation selects maintained
versions during PR10; this plan contains no stale crate or model-version pin.

Install/import verifies artifacts before activation. Queries never download a
model or open an ambient cache. Offline startup remains healthy and
lexical-complete. Compatible warmed sessions are pooled under bounded memory,
concurrency, idle, and cancellation policy. Load failure, OOM, corruption,
revocation, or incompatible pins disables the affected semantic stage without
silently selecting another model.

## Query and redundancy

Search resolves exact scope and frozen generation first, runs lexical/graph
channels first, then adds compatible semantic candidates. Fusion is stable and
explainable; exact hits keep their tier. Optional reranking is bounded to a
configured top-N candidate set and preserves the pre-rerank list byte-for-byte
when unavailable. Strict semantic requests return a typed unavailable result.

`code.redundancy` reuses the same active generation. It canonicalizes pairs,
removes self/overlapping chunks, and reports `exact_clone`,
`structural_near_duplicate`, `semantic_analogue`, or `insufficient_evidence`.
Semantic-only matches remain review candidates, never automatic rewrites or CI
violations. Disabled semantics preserves the structural baseline and ordering.

## Promotion and migration

PR10 ships a frozen sanitized corpus covering exact names, natural-language
intent, mixed queries, renamed symbols, same-name cross-scope cases, no-answer
queries, generated/vendor noise, large symbols, unsupported languages, and
incremental edits. It measures exact-hit retention, precision/recall/MRR/nDCG,
wrong-scope and no-answer error, worst strata, build/update time, p50/p95/p99,
CPU/RSS, model/vector/cache bytes, cancellation, and offline behavior.

Activation requires no scope/privacy regression, no material exact-tier
regression, demonstrated semantic gain, and declared current/10x resource
budgets. Sensitive or ineligible bytes never enter documents, artifacts,
metrics, explanations, or model-assisted routes.

Legacy vectors are never trusted or republished. Migration records
`rebuild_from_retained_eligible_code | drop_with_receipt | quarantine_unreadable`
and proves every active generation was rebuilt from canonical documents.

## Acceptance

PR10 is complete when indexing, atomic publication, lexical-preserving search, bounded
fusion/reranking/redundancy, artifact/offline lifecycle, configuration,
status/Doctor, API/CLI/MCP/dashboard parity, corpus/resource/privacy gates,
fault recovery, and rebuild-only migration pass direct tests. No separate
semantic endpoint, vector database, browser inference runtime, or model-specific
transport is introduced.
