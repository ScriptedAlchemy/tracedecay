# TraceDecay V2 Native FastEmbed Semantic Code Search

**Status:** cross-cutting evidence and implementation companion, not an independent execution authority. This plan narrows the optional semantic code-search path to one native Rust runtime and binds every implementation obligation to an existing canonical PR ID through the recognized companion headings in Section 12. It creates no Rust package, search service, vector database, transport alias, browser inference path, second model lifecycle, or duplicate slice.

**Decision:** use `fastembed` as the sole production in-process/native embedding and reranking runtime for this feature. Semantic code search is disabled by default until the frozen TraceDecay corpus proves a promoted profile improves named strata without violating exact-result, latency, memory, disk, determinism, and fallback gates. A compiled dependency, supported enum variant, successful model load, or aggregate benchmark win is not promotion evidence.

**Primary candidate profile:** `fastembed` `5.17.2` `EmbeddingModel::JinaEmbeddingsV2BaseCode` for query and code-document embeddings. The first bounded rerank experiment uses the same embedding profile followed by `RerankerModel::BGERerankerV2M3` over at most the top 25 fused candidates. `EmbeddingModel::GTELargeENV15Q` is the required general-English quantized comparator. Lexical-only remains the mandatory baseline and exact tier.

The audited FastEmbed registry reports 768 dimensions for Jina Code and 1024 for GTE Large. Candidate vectors are L2-normalized and compared with cosine similarity (equivalently dot product only after the normalization invariant is verified); the exact dimension, normalization implementation/version, finite-value check, and metric are generation identity. Runtime-observed output that disagrees with the signed manifest quarantines the artifact/generation rather than being coerced.

**Optional model-assisted profile:** a separately registered Codex Spark/app-server-style model capability, or an equivalent capability discovered through the canonical catalog, may rerank a bounded top-N candidate projection after native retrieval/fusion. It is explicit opt-in, independently evaluated, and never the availability fallback for the promoted FastEmbed embedding or native BGE reranker. The native profile remains the no-external-process acceptance baseline.

## 1. Scope and non-goals

This feature answers natural-language and mixed literal/concept questions over exact repository, checkout, worktree, ref, snapshot, and immutable code-generation scope. It retrieves files, symbols, occurrences, signatures, documentation, and bounded source slices already represented by plan 25. It may feed `code.search_symbols`, `code.context`, `search.universal`, Explorer, the Code lens, context packets, and the Search Quality Lab through the existing `TraceQueryV1` and application contracts.

It does not:

- replace exact symbol lookup, grep, phrase/BM25, graph traversal, lineage, diagnostics, or affected-test operators;
- infer repository scope, silently use the current checkout, merge incompatible vector spaces, or turn similarity into causal/impact evidence;
- embed files in the browser, use WebAssembly inference, expose raw vectors, or add `code.semantic_search` as a parallel API;
- let a query download a model, create an index, open a model file, or rebuild a generation synchronously;
- add a public model-provider plug-in interface in V2's first release;
- let a model-assisted reranker generate vectors, mutate indexes/qrels/profiles, widen retrieval, or become an implicit dependency of ordinary code search;
- import historical embedding bytes as current vectors.

The February CodeGraph plans' direct `ort`, hand-written ONNX wrapper, Nomic prefix protocol, mutable `vectors` table, and brute-force production search direction are superseded for V2. Those historical documents remain provenance; implementation must not copy their runtime, table, CLI, or MCP architecture. Holographic-memory HRR vectors are a separate memory algebra and are neither inputs nor migration candidates for semantic code search.

## 2. Canonical ownership and dependency boundaries

No new crate is admitted. Existing owners compose as follows:

| Concern | Canonical owner | Required boundary |
|---|---|---|
| Code document/chunk identity and eligible source material | plan 25, `tracedecay-code-index` | Pure deterministic `SemanticCodeDocumentV1`/`SemanticCodeChunkV1` rows derived from the exact snapshot/generation; no FastEmbed session, filesystem cache, model download, store open, or ranking. |
| Eligibility scheduling and generation requests | plan 04 projectors | Durable, change-gated requests keyed by source watermark, code-document digest, profile digest, and target generation; unchanged documents are never re-embedded. |
| Artifact/vector persistence and generation publication | plan 02 store | Immutable vector generations and profile-catalog artifact/lease state; short manifest publication transaction only. |
| Semantic AST, vector port, candidate merge, RRF, explanations, exact fallback | plan 05 query | `RepresentationQueryPort` consumes a pinned local runtime; it neither loads models nor performs network/filesystem lifecycle work. |
| Model lifecycle, authorization, operations, status | plan 09 application | Existing `representations.artifacts.*` and `representations.generations.*`; one lifecycle for embeddings and rerankers. |
| Native inference and private artifact cache | root-private `src/v2/native_semantic_runtime/fastembed.rs` | Sole production `fastembed` dependency and model/session adapter. It implements application/query/projector ports and adds no semantic ranking or store authority. |
| Capabilities and generated surfaces | plans 08, 10, 17, 21 | Existing semantic use cases only; no FastEmbed-specific public transport family. |
| Configuration and Settings | plan 20 / PR 25E | Typed profile, artifact, resource, offline, batching, rebuild, and fallback controls. |
| Explorer, Code lens, status, experiments | plan 11 / PR 31J | Generated application views; no dashboard worker/model/cache/index. |
| Corpus, qrels, promotion and regression gates | plan 15 | Immutable corpus/qrel/profile/report commands and shared experiment lifecycle. |
| Optional model-assisted reranking | plan 09 model gateway plus plan 22 capability/receipt conventions | One registered bounded read-only rerank call; no Context Scout scheduling/delivery dependency and no relevance-label authority. |
| Remote vector-generation eligibility and transfer | plan 28 | Only fully compatible immutable vector generations may sync; model artifacts, caches, and native sessions remain machine-local. |

`architecture-boundaries.toml` must register the root-private module and forbid imports of `fastembed` outside it. `tracedecay-code-index`, `tracedecay-projectors`, `tracedecay-store`, `tracedecay-query`, the dashboard, CLI, MCP, and SDKs never depend directly on `fastembed` or its transitive runtime types.

## 3. Model and runtime manifest

Every experiment, generation, query, explanation, and result pins one immutable `NativeFastEmbedRuntimeManifestV1` through the existing representation artifact/profile contracts:

- exact `fastembed` crate version and enabled Cargo features;
- resolved transitive inference-runtime version, target triple, CPU architecture/features, execution provider, thread count, determinism class, and build digest;
- exact FastEmbed enum variant plus upstream repository revision, model file names/digests, tokenizer/config digests, model dimension, maximum input length, pooling, normalization, metric, quantization, and license/notice digest;
- document schema/chunker version, language/parser/extractor versions, query/document prefix policy if the selected model requires one, truncation/overlap policy, and canonical text encoder version;
- configured embedding batch size, rerank batch size, rerank candidate cap, session-pool size, cold-load concurrency, idle lifetime, disk/RAM budget, and device;
- representation profile, ranking profile, corpus/qrel versions, source vector watermark, privacy domain/key epoch, and generation digest.

No `latest`, mutable model branch, ambient FastEmbed cache, default enum value, machine-selected thread count, or unrecorded execution-provider fallback may enter a promoted manifest. A version upgrade creates a new runtime/profile/generation and reruns the frozen evaluation. It never mutates an existing generation in place.

The `BGERerankerV2M3` enum name is not sufficient artifact identity. In the audited FastEmbed 5.17.2 registry, that variant resolves through FastEmbed's own model-code/file manifest rather than proving that the downloaded ONNX bytes are the canonical upstream `BAAI/bge-reranker-v2-m3` release. PR 2A must pin the actual resolved repository revision, files (including external data files), hashes, tokenizer/config, license/notice, and provenance chain, then either prove the cataloged artifact's accepted relationship to the upstream model card or reject/replace the candidate through FastEmbed's user-defined local-model API. A familiar enum or display label never waives artifact verification.

## 4. Code representation contract

Plan 25 adds a pure, versioned document builder over its canonical rows. Documents are stable at these grains:

- symbol definition: language, kind, qualified name, signature, documentation, and bounded body/source slice;
- file synopsis: registered language, module/package path components, declarations, imports, and bounded documentation, never an unbounded concatenated repository;
- optional occurrence/context document only when the corpus proves it adds distinct recall without duplicating definition results.

Each `SemanticCodeDocumentV1` records `CodeSnapshotId`, immutable graph generation, repository/checkout/worktree/ref/snapshot tuple, file/symbol/occurrence IDs, source spans, language/extractor/chunker versions, content digest, sensitivity/eligibility receipt, and parent entity. Each `SemanticCodeChunkV1` has a stable `chunk_id` derived from document identity, chunk-policy version, ordinal, exact sanitized byte/token bounds, and chunk digest. Canonical text construction is deterministic, length-bounded, and test-goldened. Comments, signatures, names, and bodies retain typed field boundaries so ranking can explain which representation matched.

Chunking is symbol-first. Oversized symbols split on parser-owned structural boundaries before bounded token windows; overlap is explicit and versioned. Tiny adjacent declarations may not be concatenated merely to fill a batch. Generated/vendor/minified/binary/ignored content follows plan 25's declared classification and remains an eval stratum rather than an accidental training-style corpus.

## 5. Incremental generation and storage

The generation key is the digest of exact source generation, eligible-document manifest, embedding profile/runtime/model/tokenizer/chunker pins, privacy domain/key epoch, dimension, metric, normalization, and builder version.

1. Plan 25 emits ordered document rows and digests through the existing build sink.
2. Plan 04 compares predecessor membership/digests and issues a durable representation build request only for added or changed documents; removed documents become generation tombstones.
3. The root FastEmbed adapter reuses one warmed model session per compatible runtime lease, embeds deterministic bounded batches, and streams `(document_id, chunk_id, vector, pins)` to plan 02's generation writer.
4. Build checkpoints after bounded batches and resumes only under the identical manifest. A partial generation is never queryable.
5. Store verifies row counts, dimensions, finite values, membership closure, digests, and source watermark before an atomic active-generation pointer swap. The predecessor remains available for the declared rollback/replay window.
6. File/symbol deletion, parser/chunker/model/runtime/config change, artifact revocation, or privacy/key change schedules the minimum affected rebuild; unchanged ticks do no scan, model load, or vector work.

Exact cosine scan is the benchmark reference oracle and is eligible in production only for bounded low-cardinality shards that meet the interactive gate. Plan 02 PR 6C owns the immutable logical vector-generation contract; PR 14A may materialize that contract only in its isolated benchmark harness while comparing the oracle with TraceDecay's existing vector-index adapter over identical normalized vectors. It does not publish a production generation or introduce another ANN implementation. If neither available retrieval strategy meets the corpus-scale gates, semantic code search stays disabled at that scale. FastEmbed supplies inference, not vector-store authority.

## 6. Query, fusion, and reranking

The canonical semantic path is:

1. resolve explicit `ScopeSelectorV2` and frozen code generation;
2. execute exact identifier, exact phrase, fielded BM25, and configured fuzzy/entity channels first;
3. if the selected profile is enabled and compatible, acquire the already-installed warmed FastEmbed runtime lease and embed the bounded query once;
4. retrieve semantic candidates only from generations with identical model/dimension/metric/normalization/runtime compatibility pins;
5. fuse channels using plan 05's deterministic RRF and exact-tier priority;
6. when the promoted rerank profile is enabled, send at most the first 25 fused candidates to one reused `BGERerankerV2M3` session, then apply stable score normalization and deterministic ties;
7. return canonical code entities, source generation, component explanations, coverage, and retrieval anchors.

The Jina-only profile and Jina-plus-BGE-reranker profile are independent candidates. Neither inherits promotion from the other. `GTELargeENV15Q` is a comparator, not a fallback selected at runtime. Missing features are `Absent`, never zero. Exact identifier hits remain ahead of approximate-only hits unless a separately reviewed ranking profile and exact-regression gate explicitly proves otherwise.

An optional model-assisted profile runs only after the same frozen native candidate/fusion stage. It receives at most the configured top-N safe candidate projections—stable entity/anchor, typed field excerpts, native scores, and scope/snapshot evidence—and cannot request more retrieval or hydrate arbitrary content. Its catalog route pins host/provider/model/reasoning effort plus allowed privacy/egress class, maximum input/output tokens, cost, wall deadline, cancellation, and concurrency. The receipt records requested and actual route, candidate/input/output digests, ordering or typed failure, tokens, cost, latency, policy/config/catalog versions, and experiment/query anchor. It produces only a proposed ordering with bounded score/explanation fields; no vector, qrel, judgment, profile, index, memory, hint, or task mutation is legal.

Native and model-assisted reranking are separate profiles and separate ablations. A missing/unhealthy capability, route substitution, permission refusal, deadline, cancellation, budget exhaustion, malformed output, incomplete candidate coverage, or provider error preserves the exact pre-rerank list byte-for-byte when fallback is permitted; strict mode returns a typed model-assisted-rerank-unavailable result. It never silently falls back to another external model or to the native BGE profile. Plan 22's active hinting/Context Scout may use the same registered model-capability discovery, accounting, and requested/actual-model receipt vocabulary, but scout scheduling, suggestions, and outcomes do not participate in search execution or relevance truth.

Batching is workload-aware but deterministic within a manifest. Offline generation batches documents; interactive query embedding coalesces only requests with the same profile/privacy/runtime pins and a bounded microbatch deadline. Reranking never exceeds 25 documents per query. Cancellation discards undelivered work without poisoning the shared session; timeouts do not publish partial rerank ordering as complete.

## 7. Artifact, cache, offline, and session lifecycle

PR 14E's signed representation catalog is the only source of installable artifacts. A promoted entry pins all model/tokenizer/config bytes used by FastEmbed. TraceDecay supplies its private managed cache directory explicitly; FastEmbed's ambient default cache and first-query download behavior are forbidden.

- Install/import stages and verifies every artifact before activation. Offline import uses the same manifest and digest checks.
- Activation warms the model outside query/store transactions, performs a golden inference self-test, records observed dimension/runtime/RSS, then publishes readiness.
- One bounded session pool is keyed by exact runtime/artifact/device/thread/config digest. Compatible requests reuse it; different profiles never share a session.
- Idle unload and LRU eviction skip active leases, generation builds, pinned eval/replay manifests, and current-profile artifacts.
- Offline mode performs no network calls. Installed compatible artifacts work normally; absent artifacts keep semantics disabled with typed coverage.
- Startup without any model remains healthy and lexical-complete. Prewarm is explicit and bounded, never required for daemon readiness.

Plan 28 alone owns remote distribution. A vector generation may sync only under its complete compatibility-pin and authority rules; model/tokenizer artifacts, downloaded/imported bytes, native sessions, and warm caches remain machine-local. Remote availability never relaxes local activation or selects another model.

## 8. Configuration and product surfaces

Plan 31 is authoritative for candidate models, immutable runtime/generation pins, promotion evidence, exact-tier behavior, and fallback invariants. Plan 20 is authoritative for setting IDs, writable defaults, ranges, layers, budgets, and generated forms; other surface plans reference those authorities instead of restating defaults.

Every surface reuses one four-axis state without inference: `desired` is resolved plan-20 configuration intent; `activated` is the verified artifact/profile plus published compatible generation selected by plan-09 lifecycle receipts; `effective` is the application/query route after policy, capability, compatibility, privacy, and budget evaluation for the request; `observed` is expiring daemon evidence naming the actual runtime/model/device/session/generation and outcome. A value on one axis never proves another. Model-assisted reranking uses the same axes, with discovered capability and requested/actual route evidence remaining separate from activation.

The existing public operations remain:

- `code.search_symbols`, `code.context`, and `search.universal` with the canonical semantic predicate/profile selector;
- `representations.artifacts.list|get|status|install|import|activate|deactivate|evict|verify`;
- `representations.generations.list|rebuild`;
- existing config/status/doctor, experiment, corpus, qrel, report, and profile operations.

CLI, MCP, HTTP, Rust/TypeScript/Python SDKs, and dashboard generate these same contracts. MCP remains optional. No surface accepts a model path, raw vector, arbitrary repository text, unregistered model name, or transport-local ranking toggle.

Settings shows all four axes, FastEmbed/runtime/model pins, bytes, readiness, device/threads, session warmth, generation coverage/freshness, rebuild progress, resource use, offline state, native and model-assisted rerank settings, requested/actual route health, budgets/cost, fallback count, last qualified report, and legal actions. Explorer and the Code lens show lexical versus semantic candidates, native/model-assisted fusion/rerank stages, exact-tier preservation, profile/generation, requested/actual model receipt, coverage, and a Search Quality Lab link. Browser code never embeds or reranks.

## 9. Failure and fallback contract

| Failure | Required behavior |
|---|---|
| Feature disabled or no promoted profile | Do not initialize FastEmbed or open vector generations; return the lexical result and semantic `Disabled` coverage. |
| Artifact absent while offline | Lexical-preserving fallback or typed `semantic_required_unavailable`; no download attempt. |
| Artifact/model/runtime pin mismatch or revocation | Refuse the vector generation, retain exact lexical ordering, mark semantic incompatible/unavailable, and schedule an authorized rebuild only through the lifecycle. |
| Load/self-test failure, OOM, provider failure, or session crash | Quarantine or unload the affected runtime generation according to evidence; retry policy is bounded; never switch models silently. |
| Query deadline/cancellation | Return the exact pre-semantic/pre-rerank list when fallback is allowed and name omitted stages; strict mode fails explicitly. |
| Partial/incomplete build | Keep predecessor active; never query staged rows or publish mixed dimensions. |
| Incremental checkpoint mismatch | Abandon or reconcile the staged generation; restart from the last compatible manifest checkpoint. |
| Reranker unavailable | Preserve the fused pre-rerank list byte-for-byte and record rerank omission. |
| Model-assisted capability absent, substituted, denied, timed out, cancelled, malformed, or over budget | Preserve the pre-rerank order exactly or return the explicit strict-profile error; do not try another model/native reranker and do not record the output as relevance truth. |
| Vector candidate corruption/nonfinite value | Reject the affected generation and report partial coverage; never repair inside the read. |

Every fallback test asserts entity IDs, ordering, scores/explanations for remaining stages, cursor claims, and coverage. A lexical fallback is not labeled semantic success.

## 10. Evaluation corpus and benchmark design

PR 13A/plan 15 provides the shared immutable corpus lifecycle. PR 14A adds a code-search stratum built from sanitized retained TraceDecay repositories plus synthetic cases. Split by repository family and time before tuning; near-duplicate files, forks, copied fixtures, renamed symbols, and generated variants stay in one split family to prevent leakage.

Required query strata:

- exact symbol, signature, file, error string, config key, CLI/API/tool name, and quoted literal;
- natural-language intent to exact function/type/module/file and implementation concept;
- mixed identifier plus prose, abbreviations, typo, casing, punctuation, and language-specific syntax;
- same-name symbols/files across repositories, worktrees, refs, snapshots, languages, and versions;
- renamed/moved/split/merged symbols with current versus historical/as-of scope;
- documentation/comment-to-code and code-to-related-document questions;
- graph-near but semantically wrong hard negatives, wrapper/helper duplicates, tests versus production, call sites versus definitions;
- generated/vendor/minified/fixture noise, common boilerplate, very large symbols, tiny declarations, unsupported/degraded language extraction;
- cross-repository concept queries, zero-answer queries, forbidden/out-of-scope candidates, stale/partial generations, and deleted documents;
- incremental edit cases: unchanged file, one-symbol edit, rename, delete, branch switch, dirty overlay, parser/chunker/model upgrade.

Judgments distinguish exact required, useful, context-only, redundant, stale/superseded, wrong-repository/snapshot, and no-answer. At least two human labels plus adjudication cover the promotion set; model-generated labels remain secondary evidence.

Compared variants on identical candidate pools and snapshots:

1. exact/phrase/BM25 baseline;
2. baseline plus existing fuzzy/entity/graph channels;
3. `JinaEmbeddingsV2BaseCode` semantic candidates;
4. Jina semantic candidates plus RRF;
5. Jina plus RRF plus `BGERerankerV2M3` top-25 rerank;
6. `GTELargeENV15Q` comparator under identical vector/fusion budgets;
7. registered model-assisted bounded top-N rerank over the same Jina/fused pool, when the explicit route is available;
8. every accepted candidate with semantic/native-rerank/model-assisted-rerank ablated independently.

Report Precision@1/3/5, Recall@5/10/20, MRR, nDCG@10, first-useful rank, exact-hit retention, no-answer precision, wrong-repository/snapshot rate, duplicate/redundant rate, per-language/per-intent/worst-stratum confidence intervals, and paired query-level deltas. Resource results include index build/update documents/sec, incremental touched/reused counts, vector/index bytes, model/cache bytes, cold/warm load, query embed, vector top-k, fusion, native/model-assisted rerank, end-to-end p50/p95/p99, throughput, CPU, peak RSS, session reuse hit rate, batch fill/wait, cancellation, offline startup, model-assisted tokens/cost/route failures, and deadline/budget rejection rates.

Promotion requires a reviewed predeclared threshold manifest. At minimum: zero material exact-identifier regression, zero scope/snapshot correctness regression, statistically supported improvement in Precision@3, nDCG@10, and first-useful rank on the intended natural-language code strata, no material worst-stratum regression, and compliance with plan 05's current/10x resource gates. If no variant passes, semantic code search stays disabled and only benchmark evidence/contracts may land; dormant runtime routes do not ship.

### Reproduction commands

```bash
cargo test -p tracedecay-code-index --test semantic_documents
cargo test -p tracedecay-query --test hybrid_ranking code_fastembed
cargo test -p tracedecay-query --test search_quality_eval code_fastembed
cargo test --test representation_artifact_lifecycle fastembed
cargo bench -p tracedecay-query --bench federated_topk -- --save-baseline fastembed-code
```

The harness accepts only manifest IDs, fixed seeds, repetitions, target triple, and an isolated preinstalled artifact cache. It records command, commit, dirty-state digest, corpus/qrel/candidate-pool/profile/runtime/model/index/config digests, hardware/OS, cache state, thread/batch/session settings, start/end time, and raw immutable result artifact IDs. Network is disabled during measured runs.

### Results template

| Field | Required value |
|---|---|
| Run identity | commit; corpus/qrel/pool; profile/runtime/model/index/config digests; hardware/OS |
| Variant | lexical; fuzzy/entity/graph; Jina; Jina+RRF; Jina+RRF+BGE top-25; registered model-assisted top-N; GTE comparator; ablation |
| Quality | P@1/3/5; R@5/10/20; MRR; nDCG@10; first-useful; no-answer; exact retention; wrong-scope; 95% CI |
| Worst strata | language, repository, query intent, exact identifier, rename/history, large symbol, zero-answer |
| Runtime | cold/warm load; embed/vector/fusion/native/model-assisted rerank/end-to-end p50/p95/p99; throughput; CPU; RSS; requested/actual model; tokens; cost; deadlines |
| Build/storage | full/incremental time; embedded/reused/tombstoned counts; vector/index/model/cache bytes |
| Reliability | offline, cancellation, OOM/load failure, stale generation, rebuild/resume, fallback identity |
| Verdict | rejected; benchmark-only; accepted optional; proposed default, with reviewer and report anchor |

## 11. Migration and compatibility

V1 or experimental vector bytes are rebuildable artifacts, never retained semantic evidence. PR 33G inventories every legacy code-vector table/generation and records `rebuild_from_retained_eligible_code | drop_with_receipt | quarantine_unreadable`; it does not deserialize and republish old floats into a FastEmbed generation. Source code/symbol identity and graph evidence migrate under plan 25 first, then FastEmbed generations rebuild from canonical eligible documents.

Shadow comparison pins the V1 lexical/code answers and V2 lexical baseline separately from optional semantic results. Cutover activates semantic contribution only for a promoted profile and compatible rebuilt generation; rollback disables the profile and returns to the exact lexical path without database mutation. Historical model IDs, dimensions, metrics, and scores remain provenance in migration/eval receipts, never live compatibility aliases.

## 12. Existing executable slice integration

This plan creates no slice and is supporting research/design evidence, not execution authority. The canonical PR blocks in plans 02/05/15/20/25 and the master plan own the obligations summarized below; their machine-readable block digests are the stale-work boundary. Implementers execute those canonical blocks and use this plan only for the linked rationale, benchmark protocol, and upstream evidence.

**PR 2A evidence — freeze FastEmbed and model evidence.**

Pin `fastembed` source/version/docs, resolved inference runtime, model-card revisions, licenses/notices, actual model/tokenizer/config files and digests, and evidence access dates. Enum/display names never substitute for artifact provenance.

**PR 6C evidence — immutable semantic-vector generation contract.**

Persist the staged immutable `(document_id, chunk_id, vector, pins)` generation family, verify the complete manifest, and atomically publish, retain, roll back, and retire pointers. The logical contract exists before benchmarking; no profile is activated by its existence.

**PR 14A evidence — bounded benchmark-only FastEmbed harness and promotion decision.**

Add the only pre-promotion FastEmbed executable as a root-private benchmark adapter under `src/v2/native_semantic_runtime`, compiled solely for the explicit benchmark/test target and excluded from normal and release binaries. It accepts only an exact preinstalled signed artifact/runtime manifest, runs with network disabled, uses an ephemeral plan-02-compatible logical generation writer, exposes no application/catalog/transport route, and cannot publish a production generation. Run the frozen Jina/GTE/lexical comparisons and record an accepted-or-disabled receipt. Accepted code is promoted/refactored through PR 14E; rejected code remains test-only evidence or is deleted and never becomes a dormant production dependency.

**PR 14B evidence — accepted-only fusion and explanation.**

Implement vector fusion/explanation only for a PR-14A-accepted profile. Preserve exact tiers, compatible-generation checks, and byte-stable lexical fallback.

**PR 14C evidence — independent bounded rerank evaluations.**

Evaluate Jina plus `BGERerankerV2M3` top-25 and the separately opted-in registered-model-assisted top-N profile on identical frozen pools. Each receives an independent verdict and neither is the other's fallback; this slice may finish disabled.

**PR 14E evidence — production artifact and native-runtime lifecycle.**

Only after PR 14A acceptance, deliver signed artifacts, the release-linked root-private FastEmbed runtime/session/cache lifecycle, offline import, status, and plan-04 change-gated production generation/rebuild integration. With no accepted profile, land only generic disabled contracts/catalog validation and no production inference/download route.

**PR 18D evidence — deterministic semantic code inputs.**

Emit `SemanticCodeDocumentV1`/`SemanticCodeChunkV1`, stable `chunk_id`, and ordered incremental input digests through the existing code-index build; emit no embeddings or vectors.

**PR 25E evidence — generated configuration and Settings.**

Generate plan-20-owned controls plus desired/activated/effective/observed status and legal lifecycle actions. Dashboard code owns no setting, model probe, or inferred state.

**PR 31J evidence — Search Quality product evaluation.**

Add code-search qrel, stage, profile, resource, regression, four-axis status, and report views to the shared Search Quality Lab using existing experiment/application contracts.

**PR 33G evidence — rebuild-only vector migration.**

Inventory old vectors, rebuild or drop/quarantine with receipt, and prove no historical float enters a FastEmbed generation.

**PR 35C evidence — optional semantic-query cutover.**

Activate only an accepted profile with compatible rebuilt generations and rollback receipts; otherwise keep the lexical route authoritative and semantic coverage disabled.

**PR 37C evidence — retire parallel vector implementations.**

Delete superseded direct-runtime, model wiring, vector scan/query/storage, cache, and scheduler paths after archived parity/rollback evidence proves the canonical route independent.

The executable order is: plan 25 PR 18D and plan 02 PR 6C contracts -> PR 14A benchmark/accept-or-disable -> accepted-only PR 14E artifact/runtime lifecycle plus plan-04 production incremental vector scheduling -> PR 14B fusion -> PR 14C native/model-assisted rerank evaluation and optional activation -> plan 20 PR 25E plus generated API/CLI/MCP/SDK/dashboard surfaces -> PR 31J product evaluation -> plan 25 PR 33G rebuild/drop migration -> PR 35C cutover -> PR 37C retirement. A disabled PR 14A disposition short-circuits all production-runtime/vector/fusion/rerank activation work while preserving benchmark/contracts and lexical search.

## 13. Primary research manifest

Accessed 2026-07-12. Implementation refreshes exact upstream revisions/digests before PR 2A freezes the evidence; URLs do not substitute for pinned artifacts.

| Source | Primary fact used by this plan | Disposition |
|---|---|---|
| [`fastembed` 5.17.2 crate docs](https://docs.rs/fastembed/5.17.2/fastembed/) | Native synchronous local inference; `TextEmbedding`, `TextRerank`, cache and user-defined local-model APIs. | Sole production runtime candidate; TraceDecay overrides ambient download/cache lifecycle. |
| [`EmbeddingModel` 5.17.2](https://docs.rs/fastembed/5.17.2/fastembed/enum.EmbeddingModel.html) | Contains `JinaEmbeddingsV2BaseCode` and `GTELargeENV15Q`. | Exact enum/runtime pins for primary and comparator. |
| [`TextInitOptions` 5.17.2](https://docs.rs/fastembed/5.17.2/fastembed/type.TextInitOptions.html) | Exposes model, cache, execution-provider, maximum-length, and intra-thread initialization controls. | Lowered from the immutable runtime manifest; never ambient defaults. |
| [`TextRerank` 5.17.2](https://docs.rs/fastembed/5.17.2/fastembed/struct.TextRerank.html) and [`RerankInitOptions`](https://docs.rs/fastembed/5.17.2/fastembed/type.RerankInitOptions.html) | Local bounded reranking with explicit model/options/batch input. | Used only for an accepted top-25 rerank profile. |
| [`fastembed` 5.17.2 reranker registry source](https://docs.rs/crate/fastembed/5.17.2/source/src/models/reranking.rs) | The enum variant resolves to a concrete model-code and model-file set that must be inspected rather than inferred from the enum label. | PR 2A freezes the actual resolved artifact/provenance/license or rejects the candidate. |
| [`jinaai/jina-embeddings-v2-base-code`](https://huggingface.co/jinaai/jina-embeddings-v2-base-code) | Code-focused model card, supported languages, long-context/code-search intent, Apache-2.0 declaration. | Primary candidate; performance remains unproven until TraceDecay corpus evaluation. |
| [`BAAI/bge-reranker-v2-m3`](https://huggingface.co/BAAI/bge-reranker-v2-m3) | Multilingual cross-encoder reranker model card and scoring semantics. | Bounded top-25 candidate only; independent promotion required. |
| [`Alibaba-NLP/gte-large-en-v1.5`](https://huggingface.co/Alibaba-NLP/gte-large-en-v1.5) | General-English long-context embedding model and Apache-2.0 declaration. | FastEmbed `GTELargeENV15Q` comparator; never silent fallback. |

## 14. Definition of done

- [ ] `fastembed` appears in production dependencies only through the root-private runtime module; no new crate/package, browser/WASM runtime, second cache, or second lifecycle exists.
- [ ] Code representation documents are deterministic, scope/snapshot exact, incremental, receipt-bound, and rebuildable; unchanged inputs perform zero embedding work.
- [ ] Model/runtime/tokenizer/chunker/dimension/metric/normalization pins follow every generation, query, explanation, experiment, result, and migration receipt.
- [ ] Jina, Jina+BGE top-25, and GTE comparator runs are reproducible on frozen pools with full quality/resource/worst-stratum reports and independently reviewed verdicts.
- [ ] Disabled/no-model/offline/revoked/OOM/cancelled/incompatible/partial-build states preserve lexical results exactly or fail with the typed strict-mode error; no model is selected or downloaded silently.
- [ ] Generated config/UI/API/CLI/MCP/SDK/status/doctor surfaces share existing use cases and expose truthful profile, readiness, coverage, rebuild, and fallback state.
- [ ] PR 33G proves every historical code vector is rebuilt, dropped, or quarantined with receipt and no old float enters a FastEmbed generation.
- [ ] No profile becomes default without the reviewed promotion report; failure to beat the baseline leaves semantic code search disabled.
