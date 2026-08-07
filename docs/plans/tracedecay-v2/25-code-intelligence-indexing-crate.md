# TraceDecay V2 Code Intelligence Indexing Plan

## Status / role

Status: active code-intelligence implementation and product-test authority. The current
checkout contains callable deterministic indexing, chunk/lineage, Git/impact,
diagnostic/test, exact, lexical, and graph paths. Delivery is not complete until the
direct behavioral tests, Plan 15 Linux evaluation, and normal CI pass.
The product must deliver one complete single-root vertical:
deterministic code indexing, immutable generations, generation-bound Git/
diagnostic/test evidence, and accepted exact/lexical/graph retrieval. Start as
a focused module; extract `tracedecay-code-index` only when independent reuse,
dependency isolation, and same-host compile measurements justify the crate
boundary. Production indexing and retrieval paths emit incremental, no-op,
generation, and resource measurements directly to the end-to-end performance
journey.
Generation-bound diagnostics compose with the daemon gateway defined by
[Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
[Plan 15](15-search-quality-evaluation-and-retrieval-research.md) exclusively
owns retrieval-research design, corpus/label policy, quality metrics, candidate
profile comparison, thresholds, and activation recommendations. Plan 25 implements the
versioned lexical/chunk contracts and emits measurements; it does not tune or
activate retrieval policy.

**Incremental-runtime correction (2026-07-27).** Foreground code queries no
longer wait behind an in-flight scheduler refresh: each mounted worktree retains
its last complete immutable generation, a busy scheduler serves that generation,
and successful reconcile atomically replaces it. Shutdown cancellation is
checked before and during snapshot capture so work that cannot publish exits
cooperatively. Direct regressions cover both behaviors.

This closes serve-during-refresh and cooperative-shutdown behavior, not full
acceptance or freshness cadence. A live profile was observed 237 minutes stale,
and the index reported refresh beginning only after 285 minutes while the plan
reconciliation was checked. The Plan 25 owner must diagnose hint/reconcile
cadence and complete the event-to-ready measurements; serving an old complete
generation truthfully is not permission to leave it stale indefinitely.

(Update 2026-08-07: both assignments in that paragraph have code repairs at
tip. The hint/reconcile cadence diagnosis produced a three-tier freshness
ladder run at query admission rather than trusting a hint to arrive —
`fix(daemon): close code-index freshness cadence` (cbac4ae64e); tier-1 is the
cheap `.git`-metadata fingerprint
(`src/daemon/code_index_scheduler/identity.rs:150`) and tier-2 is the bounded
staleness reconcile, with direct regressions covering the no-watcher and
HEAD-moved paths at `src/daemon/code_index_scheduler/tests.rs:4446-4588`
(`threshold_expiry_reconciles_out_of_band_write_without_watcher`,
`identity_move_reconciles_and_never_mixes_identity`). The event-to-ready
measurements are implemented as receipts in
`src/daemon/code_index_scheduler/cadence.rs` —
`feat(code-index): measure event-to-ready arrival, wait, and service`
(325ea665e4) — which records arrival, dequeue, and terminal instants
separately so queue wait and service time are distinct, and withholds latency
as `CodeIndexArrivalV1::Unavailable` rather than emitting a false zero sample.
What is NOT closed: the 237-minute observation was an operational measurement
on a live profile, and no equivalent re-observation has been taken since these
landed. The mechanism to prove cadence now exists; the proof does not.)

Plan 25 owns code-generation, chunking, graph, and generation-bound evidence
semantics. Plan 15 owns quality evaluation. Plan 31 consumes the tested
consumer of tested chunks and lexical/graph fallback behavior; application,
transport, and dashboard plans consume the tested operations later. None of
those consumers must reproduce an old module path, Rust type spelling, suite
spine, fixture filename, or benchmark script.

## Outcome

TraceDecay builds deterministic, immutable code-intelligence generations from
sanitized repository snapshots. Incremental builds reuse unchanged work,
preserve symbol lineage, and attach Git, diagnostics, and tests to the exact
source generation they describe. The retrieval service then serves those generations through the
Plan 15 exact/lexical/graph contracts with a non-demotable exact tier,
deterministic compact-candidate fusion, late hydration, and a versioned
lexical profile whose named fallback subpayload semantic retrieval preserves byte-for-byte
when semantics are unavailable.

## Owns

- Versioned tree-sitter grammar registration and deterministic language extraction.
- One versioned language descriptor per language, shared by extraction,
  structural search, outline, rewrite, analyzer routing, and host LSP
  projection.
- Canonical symbol, occurrence, relationship, diagnostic, and test-attribution records.
- Storage-neutral `CodeSearchDocumentV1`, `CodeSearchChunkV1`,
  `CodeSearchChunkId`, `CodeSearchChunkGrainV1`,
  `ChangedCodeChunkSetV1`, `CodeChunkProjectionReceiptV1`, and
  `CodeIndexCapabilityManifestV1` values. They are immutable logical records,
  not rows coupled to a lexical table, vector table, or vendor index.
- Deterministic symbol-signature, symbol-body, symbol-member, file-preamble,
  and bounded file-window chunks tied to one code generation. These chunks are
  the replayable source for lexical and later model/version-specific
  projections; embeddings never become source or symbol authority.
- Ordered changed/reused/deleted chunk manifests and projection receipts that
  let downstream projectors prove exactly which generation-bound chunks they
  consumed, skipped, replaced, or removed.
- Canonical raw quantifier inputs: decision/control-flow facts, typed
  dependency/test/change relations, extraction coverage, ambiguity, and
  language-validation revision. Preserve raw evidence sufficient to recompute
  compatible Plan 26 descriptors without reparsing; this plan does not define
  a universal quality score or outcome policy.
- Generation-exact mappings from native Git file and hunk evidence to canonical
  symbols, callers, change-hazard evidence, and affected-test candidates.
- Code-specific adapters that emit Plan 15 `CompactCandidate` values for the
  independent `ExactLiteral`, `Lexical`, and `Graph` lanes from one frozen code
  generation. Plan 15 owns the common candidate, contribution, fusion,
  diversity, rerank, hydration, cursor, and evaluation types; this plan owns
  only the code-generation evidence carried by those adapters.
- Task/work composition may select exact code-generation, occurrence,
  relationship, diagnostic, and test-attribution evidence from an authorized
  Plan 24 `TaskId` only through Plan 13 anchors and typed application joins.
  The index stores no task identity, task summary, or copied task evidence.
- Content-addressed incremental reuse and bounded sanitized dirty-worktree
  indexing overlays captured from repository state. Unsaved per-client LSP
  document overlays are separate Plan 35 daemon session state.
- Logical generation planning, sealing, digests, and lineage evidence.
- Native historical source acquisition sanitized into the final logical model;
  old TraceDecay graph-store bytes are never conversion input.

## Does not own

- Filesystem watching, repository reads, snapshot coalescing, or redaction; capture owns those.
- Database connections, generation files, transactions, manifests, pointers, or publication; store owns those.
- Projector scheduling, retries, or checkpoints.
- Query ranking, semantic embedding inference, UI, or public transport bindings.
- A second code-specific retrieval kernel, code-only fusion profile, or
  code-only application service. This plan implements code adapters and the shared
  query stages under Plans 05/15; the application delivery later composes application use cases.
- Retrieval-research policy, fusion weights, diversity limits, candidate
  budgets, corpus labels, metric interpretation, or profile promotion; Plan 15
  owns those decisions and Plan 05 owns retrieval/fusion implementation.
- A required physical table layout. Canonical chunks, lexical postings, graph
  evidence, vectors, and receipts may use separate store-owned representations
  and join only through stable typed identities and generation manifests.
- Analyzer executable commands or settings, which remain configuration-owned
  by Plan 20.
- A host-facing analyzer broker; the Plan 35 daemon gateway is the sole broker
  presented to LSP hosts.
- A second repository identity, intake queue, or write path.
- Git object storage, revision traversal, status, blame, rename detection, or a
  patch/diff engine. Native Git owns those semantics; this boundary indexes and
  joins only receipt-bound evidence supplied by capture/application ports.

## Required behavior

### Sanitized intake

- Accept only receipt-bound sanitized snapshots carrying repository, checkout, worktree, ref, source revision, sanitizer revision, and content identity.
- Reject missing, stale, mixed-snapshot, or unsanitized input before parsing.
- Treat deletions, renames, ignored files, binary files, generated files, and unsupported languages explicitly.

### Deterministic extraction

- Select grammar, aliases, extensions, expando behavior, and extractor revision
  through one versioned registry. Duplicate language tables and parser
  acquisition paths are forbidden.
- The same canonical descriptor supplies extension, language-ID, root-marker,
  and capability facts for analyzer routing and host LSP projection. It does
  not absorb configuration-owned executable commands or settings.
- Acquire one Tree-sitter parser from that descriptor. Extraction and the
  in-process `ast-grep-core` structural-match/outline/rewrite kernel share its
  pinned grammar and source generation; no host `ast-grep` binary is authority.
- Produce stable canonical rows and digests for identical input, registry, and extractor revisions on every supported host.
- Preserve parse errors and unsupported constructs as evidence; never invent successful structure.
- Record descriptor, grammar, and extractor revisions; parse outcome; parsed,
  error, and unsupported ranges; timeout/cancellation; content digest; edge
  authority (`syntax_exact | name_resolved | compiler_or_lsp_resolved |
  dynamic_observed | heuristic_candidate | unknown_unsupported`); ambiguity;
  and coverage. Bounded traversal or extraction caps propagate as partial.
- Keep language-specific logic behind a small extractor interface while sharing identity, lineage, and output contracts.
- Keep parser and grammar dependencies behind the code-intelligence ownership
  boundary so unrelated domain, store, application, and adapter checks do not
  compile them. Feature groupings reflect shipped language capability, not a
  convenience meta-feature that silently expands unrelated builds.
- Record same-host clean, warm incremental, and no-op check/test compilation
  for the core registry and representative grammar groups. If an extractor-only
  change repeatedly rebuilds unrelated grammar bindings, use that evidence to
  refine module, feature, or crate boundaries without weakening default product
  capability.
- Structural results report deterministic file/span order, parse coverage,
  unsupported regions, and bounded errors. Pagination cursors bind query,
  descriptor, generation, and ordering; cancellation cannot publish partial
  extraction or mutation state.

### Code-search chunk and projection contract

The required contract is behavioral: generation-bound storage-neutral
documents and chunks, deterministic identity and coverage, explicit
changed/reused/deleted manifests, projection compatibility, receipts, and
truthful invalidation. The following Rust names and layout are a historical
design sketch, not artifact-name parity requirements. The delivery may realize the
contract through current owner-approved names and paths when direct contract
regressions prove all fields and invariants below.

The original design placed stable values in the domain layer and the initial
implementation in the root package, with optional later crate extraction. That
physical placement remains evidence for boundary review, not a required
reconstruction.

The domain contract is:

```rust
pub struct CodeSearchDocumentV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub eligibility: CodeSearchEligibilityV1,
    pub chunk_ids: Vec<CodeSearchChunkId>,
}

pub enum CodeSearchChunkGrainV1 {
    SymbolSignature,
    SymbolBody,
    SymbolMember,
    FilePreamble,
    FileWindow,
}

pub struct CodeSearchChunkAnchorV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
    pub parent_chunk_id: Option<CodeSearchChunkId>,
    pub source_span: SourceSpan,
    pub grain: CodeSearchChunkGrainV1,
    pub ordinal: u32,
}

pub struct CodeSearchChunkV1 {
    pub id: CodeSearchChunkId,
    pub anchor: CodeSearchChunkAnchorV1,
    pub content_digest: ContentDigest,
    pub language_descriptor_revision: LanguageDescriptorRevision,
    pub chunker_revision: ChunkerRevision,
    pub sanitizer_revision: SanitizerRevision,
    pub sensitivity: SensitivityDecision,
    pub exact_terms: Vec<ExactTechnicalTermV1>,
    pub sanitized_text: BoundedSanitizedText,
}

pub struct ChangedCodeChunkSetV1 {
    pub from_generation: Option<CodeGenerationId>,
    pub to_generation: CodeGenerationId,
    pub manifest_digest: ManifestDigest,
    pub added_or_changed: Vec<ChangedCodeChunkV1>,
    pub deleted: Vec<ChangedCodeChunkV1>,
    pub reused: Vec<ChangedCodeChunkV1>,
}

pub struct ChangedCodeChunkV1 {
    pub chunk_id: CodeSearchChunkId,
    pub prior_digest: Option<ContentDigest>,
    pub current_digest: Option<ContentDigest>,
}

pub struct ProjectionBatchRequestV1 {
    pub request_digest: ManifestDigest,
    pub changes: ChangedCodeChunkSetV1,
    pub previous_projection_key: Option<ProjectionKeyV1>,
    pub target_projection_key: ProjectionKeyV1,
    pub replay_reason: ProjectionReplayReasonV1,
}

pub struct CodeChunkProjectionReceiptV1 {
    pub projection_key: ProjectionKeyV1,
    pub request_digest: ManifestDigest,
    pub prior_generation: Option<CodeGenerationId>,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub chunk_id: CodeSearchChunkId,
    pub prior_chunk_digest: Option<ContentDigest>,
    pub current_chunk_digest: Option<ContentDigest>,
    pub operation: ProjectionOperationV1,
    pub outcome: ProjectionOutcomeV1,
    pub output_digest: Option<ContentDigest>,
}

pub struct ProjectionBatchReceiptV1 {
    pub target_projection_key: ProjectionKeyV1,
    pub request_digest: ManifestDigest,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub receipts: Vec<CodeChunkProjectionReceiptV1>,
    pub reused_count: u64,
    pub publication_digest: ManifestDigest,
}
```

- `CodeSearchDocumentV1` is one generation-bound file manifest and the
  scheduling/checkpoint unit; chunks are the projection and receipt unit.
  Document eligibility changes expand into explicit chunk changes before a
  projector runs.
- `CodeSearchChunkId` is the digest of repository identity, file logical
  identity, optional symbol logical identity, grain, structural split path (or
  fallback window start/size), and chunker revision. Generation and content
  digest are excluded: `(CodeGenerationId, CodeSearchChunkId)` is the exact
  occurrence key, while a digest change classifies an upsert and a move/rename
  or structural-boundary change classifies delete-plus-add. Extractor
  enumeration order and mutable line numbers cannot affect identity.
- `ProjectionKeyV1` contains projection kind, projection schema revision, and a
  canonical profile digest. Plan 31's `EmbeddingProjectionKeyV1` is the typed
  semantic profile whose canonical digest occupies that field; adapters cannot
  define a second projection-key identity.
- Symbol signatures and bodies are separate grains. Members become child
  chunks only when the language descriptor identifies stable member spans.
  Oversized bodies split on deterministic structural boundaries; if none are
  available, fixed byte windows with pinned size/overlap are used. File
  preambles cover imports/module documentation. File windows cover otherwise
  unowned sanitized ranges. Every eligible sanitized byte is covered by a
  declared chunk or an explicit unsupported/excluded range.
- `exact_terms` classifies whole symbols, qualified names, paths, compiler and
  runtime error codes/text, CLI flags, tool names, and configuration keys.
  Whole exact terms and language-profiled subtokens are distinct fields. This
  is extraction evidence only; Plan 05 applies Plan 15's protected lexical
  policy.
- `src/code_index/chunks.rs` builds chunks and their parent/child hierarchy.
  `src/code_index/incremental.rs` compares ordered manifests by typed ID and
  digest. `src/code_index/projection.rs` exposes
  `CodeChunkProjectionSink::project_changed_chunks(ProjectionBatchRequestV1)
  -> ProjectionBatchReceiptV1` and validates returned receipts.
  `src/code_index/capabilities.rs` emits
  `CodeIndexCapabilityManifestV1`.
- The mandatory base capability manifest pins code generation, chunk schema/chunker and
  language-descriptor revisions, available grains and exact-term fields,
  supported languages, graph edge-authority classes, privacy domain/key epoch,
  source coverage, exclusions, partial states, and manifest digest. Consumers
  must reject a missing, incompatible, mixed-generation, or unauthorized
  base manifest before candidate production. Plan 31's optional semantic
  manifest augments this base; its absence cannot block authorized
  lexical/graph retrieval.
- A no-op generation emits empty `added_or_changed` and `deleted` sets plus
  explicit `reused` counts and causes zero projection calls. An edit reprojects
  only changed symbol chunks, affected ancestors/file windows, and explicit
  deletions. Grammar, chunker, sanitizer, identity, or privacy incompatibility
  emits a full-rebuild reason rather than disguising all chunks as ordinary
  edits.
- Projection receipts are deterministic apart from store-owned operational
  timestamps, which are excluded from receipt identity and digest. Publication
  rejects duplicate, missing, extra, cross-generation, wrong-digest, or
  wrong-projection-key receipts. Failed or partial receipt sets remain
  inspectable but cannot activate a projection generation.
- Invalidation is field-specific: source content, language descriptor,
  extractor, sanitizer, sensitivity, or chunker changes rebuild canonical
  documents/chunks; projection-profile changes replay retained eligible chunks
  without parsing; query/fusion/diversity/rerank/hydration profile changes
  invalidate only query/session caches. Privacy-domain or key-epoch changes
  rebuild canonical eligibility when policy output changes and always create a
  new projection plus zero cross-epoch cache reuse.

### Generations and incremental reuse

- Build one immutable logical generation from one fenced snapshot.
- Treat every change signal as a wake-up hint, never as the changed-file
  authority. Because TraceDecay's edits are agent-driven, host after-file-edit
  hooks are the primary hint source; a lazy three-tier freshness ladder
  (per-query `.git` metadata fingerprint, configurable bounded-staleness
  threshold, identity re-resolution backstop) catches external or out-of-agent
  mutations without a standing watcher. A recursive `notify` watcher (with its
  recommended file-ID cache to coalesce bursts and rename pairs) remains an
  off-by-default opt-in fallback for non-agent-driven setups. Reconcile every
  hint against native `gix` index/worktree/tree status; dropped-event or
  overflow signals trigger one bounded reconciliation before generation
  planning.

  **Dated amendment (2026-08-07, recorded decision — supersedes the
  opt-in-fallback text above).** The recursive `notify` working-tree watcher
  (the "#80 working-tree watcher") was removed rather than shipped
  off-by-default; no opt-in config key exists to enable a working-tree
  watcher (only `user.watcher_debounce_ms.v1` tunes the replacement below).
  The v6.x `notify-debouncer-full` watcher recursively watched the working
  tree and drowned on monorepo `node_modules`/`target` churn. Its
  replacement — landed and unconditional — is an always-on unix-only
  git-metadata watcher (`src/daemon/git_watch.rs`, designs D3/D5) that
  watches only `HEAD`, `packed-refs`, `refs/`, and `worktrees/` under
  `<git_common_dir>` (~5-20 inotify watches per repository) and "never fires
  on a source-file edit," plus a backstop timer
  (`src/daemon/git_watch/backstop.rs`) that submits freshness requests on a
  cadence independent of watcher liveness, feeding the same three-tier
  freshness ladder described above. The full rationale for why this design
  is safe without a working-tree watcher lives in the `git_watch.rs` module
  doc comment ("Why this is safe (unlike the removed #80 working-tree
  watcher)"); this note is the plan-text pointer to it. Intent (no standing
  working-tree watcher; out-of-agent edits still caught) holds via the
  ladder's tier-2 staleness threshold and the backstop; the letter of this
  bullet (an opt-in recursive fallback existing at all) does not.
- Use the existing ignore-aware parallel walker for cold discovery. Warm
  batches hash only reconciled candidate paths and reject duplicate event or
  save-without-change work by content plus descriptor digest before parsing.
- Retain the admitted prior Tree-sitter tree for each saved-file content
  identity. Apply `InputEdit`, parse with the old tree, and use
  `changed_ranges` to bound extraction. Re-fetch canonical nodes after parsing;
  Tree-sitter object identity and shared internal nodes are never lineage or
  product identity.
- Reparse only changed sanitized content or descriptor inputs. Reuse file and
  symbol results only when content, grammar, extractor, identity, and sanitizer
  inputs match; recompute relation and attribution rows only for dependency
  closures invalidated by versioned evidence.
- Keep generation identity exact per repository/worktree/ref/snapshot.
  Content-addressed parse and chunk artifacts may be physically reused across
  worktrees only when source content, language descriptor, extractor,
  sanitizer, privacy domain, and key epoch match. Reuse never merges worktree,
  occurrence, authorization, generation, or lineage identity.
- Coalesce superseded batches by exact worktree and content frontier. Bound
  queue depth/bytes and parser/publication concurrency, preserve fair progress
  across active worktrees, and cancel a build whose fenced snapshot can no
  longer publish.
- Report parse work, resolution work, invalidation fan-out, extracted-row
  reuse, and conservative full-rebuild fallback separately. Tree-sitter node
  reuse is performance evidence, never product identity or lineage.
- Force a full rebuild for incompatible schema, grammar, identity, or privacy changes and for quarantined corruption.
- Seal the generation before handing rows and the expected digest to the store publication port.
- Never mutate a published generation or substitute the active checkout for the selected snapshot.

### Identity and lineage

- Generation-local occurrence identity is exact. Logical identity remains
  stable only while its declared repository, language, qualified-structure,
  and source-evidence tuple is unchanged.
- Record rename, move, split, merge, and structural-continuity candidates with
  method, evidence, confidence kind, alternatives, and abstention.
  Tree-sitter object reuse, path, line, qualified-name similarity, or embedding
  similarity never proves lineage.
- Keep ambiguous lineage explicit; do not silently merge unrelated symbols.

### Git evidence joins

- The anchor delivery anchors exact commit, tree, blob, index, or captured-worktree evidence.
  Code intelligence consumes native Git status, history, blame, and working/staged/range diff
  results through typed ports and addresses hunks with `HunkRef`; it never
  reconstructs patches from indexed rows.
- Map a hunk to symbols only when repository identity, path/content identity,
  source side, and code generation match. Tree-sitter supplies canonical syntax
  spans; `ast-grep-core` supplies requested structural-pattern matches; neither
  replaces Git diff/blame/history nor creates a parallel symbol graph.
- Derive callers, hazards, and affected-test candidates from the canonical graph
  and test-attribution evidence with bounded traversal and explicit evidence
  class. A hunk-to-symbol match does not prove runtime impact or test execution.
- Preserve separate Git/capture provenance and watermarks, code-index generation
  and graph provenance, and diagnostic/test provenance. Joined results expose
  every relevant watermark plus mismatch, staleness, and partial coverage.

### Diagnostics and tests

- Attach compiler and language-server diagnostics to exact file and symbol
  occurrences only within the matching sanitized clean generation and content
  digest.
- Retain producer kind and identity, analyzer and configuration revisions,
  evidence class, freshness, and clearing or supersession provenance.
- Keep clean-generation persistence separate from unsaved LSP overlays.
  Overlays remain ephemeral daemon session state and become durable only after
  saved content passes the normal capture and generation pipeline with the same
  digest.
- Stale, cleared, historical, or cross-snapshot diagnostics remain evidence but
  cannot publish as current. Plan 35's daemon gateway is the only host-facing
  analyzer broker and cannot create a parallel diagnostic authority.
- Map test definitions and runs to the generation, source revision, and candidate production symbols they cover.
- Version test definitions, observed execution, coverage, candidate-production
  edges, and predictive selection independently.
- Distinguish conservative dependency candidates, observed-coverage
  candidates, predictive ranked candidates, stale evidence, and
  unknown/unsupported attribution. No candidate mode proves execution,
  correctness, or universal safety.

### Exact, lexical, graph, Git, and diagnostic retrieval

- Plan 25 lands the shared Plan 15 types in
  `crates/tracedecay-domain/src/retrieval.rs` and the query implementation in
  `src/query/retrieval/`. Code search does not define parallel
  `RankedChannelList`, fusion-profile, contribution, candidate, cursor, or
  hydration types under `code_intelligence/search.rs`.
- `src/query/retrieval/exact.rs` consumes only whole exact technical terms and
  a central `ExactAdmissionProof`. `src/query/retrieval/lexical.rs` consumes
  whole-term and language-profiled subtoken postings independently.
  `src/query/retrieval/graph.rs` emits generation-bound code anchors and
  ordered path evidence without copying graph rows into a search corpus.
- Exact identifiers, qualified names, paths, quoted phrases, compiler/runtime
  errors, CLI flags, tool names, configuration keys, and commit identifiers
  form the non-demotable exact tier. An approximate, graph-only, or later
  semantic candidate cannot precede an eligible exact result.
- This indexing boundary is explicitly single-root. "Federation" in Plan 15 means composing
  independent evidence lanes within one authorized root; Plan 16's multi-root
  scope-set resolution, per-shard continuations, and cross-root rank fallback
  remain owned by [Plan 16](16-cross-project-repository-worktree-scope.md).
- `src/query/retrieval/{fusion,dedupe,diversity,hydrate}.rs` operates on compact
  candidates. The promoted lexical profile uses deterministic fixed-point
  contributions, complete comparator provenance, source/file caps, and bounded
  late hydration. RRF may be evaluated, but no constant or weight is production
  authority before Plan 15 accepts it.
- The canonical exact/lexical/graph fallback is the complete accepted result,
  result, including IDs, order, contributions, explanations, coverage, and
  cursor bytes. Those fields form Plan 15's named fallback subpayload.
  Semantic retrieval must preserve that subpayload byte-for-byte whenever the
  rerank stage is disabled, unavailable, rejected, or cancelled; a typed
  semantic/rerank outcome may exist only outside its digest and cursor identity.
- Plan 36 owns native read-only status, working/staged/range diff, history,
  blame, rename, binary, merge, and `HunkRef` semantics. Code adapters join those
  typed results to exact code-generation symbols, callers, hazards,
  diagnostics, and affected-test candidates; they never reconstruct Git
  objects or patches from indexed rows.
- Generation-bound diagnostics are persisted only for matching sanitized clean
  content with producer/configuration provenance and clearing/supersession
  state. The canonical query adapter reads this evidence; Plan 35's live analyzer
  broker and unsaved overlays remain later daemon-gateway work.
- Every lane reports freshness, examined/eligible/excluded/capped/unknown
  coverage, cancellation, and partial/unavailable state independently. Missing
  authority is capability-reported, never simulated or replaced with a
  heuristic lookalike.

### Final-shape initialization

- Consume only sanitized native-source logical batches through the canonical
  capture boundary.
- Preserve source generation and acquisition provenance, build deterministic
  final identities, and verify counts and digests before publication.
- Never open or convert an old TraceDecay database from the indexer; a
  non-final store is rejected with `ResetRequired`.

## Behavioral delivery and verification

Delivery remains unfinished. The checkpoints below describe required product
behavior and direct evidence. Paths, symbol names, test-module registration,
fixture filenames, benchmark entrypoints, and old acceptance-spine names are
historical implementation suggestions only. Completion uses callable
index/search behavior, direct regressions, the Plan 15 Linux evaluation, and
normal CI.

1. **Contract and intake behavior:** define typed sanitized snapshots,
   generation manifests, extraction batches, lineage candidates, test
   attribution, and typed references to Plan 35 diagnostic evidence. A
   validated intake rejects unsanitized, stale, mixed, or malformed input.
   Direct contracts cover serialization, malformed values, intake rejection,
   and authority separation. In the same checkpoint, provide Plan 15's common
   retrieval contract and decide the Plan 05 physical query-crate extraction
   from measured reuse and compile evidence. That decision changes location
   only, not contracts.
2. **Language registry, extraction, and chunks:** implement one versioned
   descriptor authority and one bounded extractor interface; descriptors, not
   ad hoc extractors, select grammars and capabilities. Direct regressions
   cover aliases, parse errors, cancellation, caps, every chunk grain, exact
   terms, structural fallback, unsupported ranges, ordering, and capability
   digests.
3. **Independent authorities:** implement three disjoint authorities:
   Plan 36 read-only Git status/diff/history/blame/`HunkRef` ports and native
   adapters; generation-bound clean diagnostic persistence/query fixtures; and
   the versioned Plan 15 real sanitized corpus, exact-admission oracles,
   authorization canaries, and raw Linux baseline instrumentation. Synthetic
   fixtures may prove contracts but cannot replace the real developer eval.
4. **Generations, incrementality, and lineage:** implement immutable planning,
   sealing, incremental change classification, and evidence-backed lineage.
   Direct regressions cover duplicate watcher events, save-without-change,
   dropped-event reconciliation, staged-only and worktree-only edits, no-op,
   one-symbol and preamble edits, rename, move, deletion, split/merge,
   ambiguity/abstention, cross-worktree content reuse without identity reuse,
   chunker/grammar/privacy invalidation, sealing, and mixed-snapshot rejection.
5. **Git, diagnostics, and test joins:** implement generation-exact joins with
   independent provenance. Direct regressions cover working/staged/range
   hunks, mismatch/binary/rename/deletion cases, current/stale/cleared
   diagnostics, and every declared attribution evidence class.
6. **Projection boundary and final-shape admission:** prove receipt conformance with
   reordered, duplicate, missing, extra, wrong-generation, and wrong-digest
   fixtures without a model runtime or concrete store adapter. Prove native
   acquisition counts, digests, duplicates, unsupported rows, cancellation,
   final-shape rejection, and the no-database-open boundary.
7. **Exact, lexical, and graph retrieval:** implement independently disableable
   exact and lexical lanes; graph consumes only generation-matched Plan 25
   evidence. Quality fixtures and direct regressions cover exact admission,
   whole identifiers versus subtokens, phrases, errors, paths, bounded fuzzy
   terms, field filters, shuffled producer order, fixed-point fusion,
   source/file caps, pagination, denial non-interference, partial coverage, and
   rank-before-hydrate behavior.
8. **Lexical evaluation and activation:** run Plan 15's direct comparisons on
   the sanitized Linux workload and report `pass`, `fail`, or `pending`.
   Preserve the versioned exact-tier rules, profile digest, and named
   fallback-subpayload bytes as semantic-retrieval inputs; do not create a holdout, run
   manifest, owner receipt, or promotion packet.
9. **Measurement and verification:** use a reproducible checked-in Linux
   workload and retained raw samples to record clean, warm one-file, deletion,
   no-op, chunker/model-key replay, and
   incompatible full-rebuild cases at current and 10x corpus sizes. Report
   files parsed, chunks added/changed/deleted/reused, projection calls, bytes,
   wall time, CPU, and peak RSS separately; end-to-end performance work owns
   product resource budgets and Plan 15 owns quality interpretation. The
   workload revision records exact file/byte/chunk counts, real content and
   descriptor digests, language strata, seed, runtime/hardware summary, and
   cache state. Retain the raw samples needed for every reported statistic.
   Run focused direct regressions, architecture boundaries, migration/privacy
   tests, the applicable all-feature CI, and the Plan 15 comparison before
   declaring code intelligence complete.

## Acceptance

- Identical sanitized fixtures produce byte-identical logical rows and generation digests across repeated and supported-host runs.
- Repeated runs and shuffled extractor output produce byte-identical chunk
  manifests, capability manifests, chunk IDs, parent/child links, and receipt
  identity digests.
- Fixtures prove line-number and extractor-order changes do not alter
  `CodeSearchChunkId`, a content-only edit keeps the logical chunk ID and
  changes its digest, and move/rename or structural split-boundary changes emit
  explicit delete-plus-add entries.
- Every eligible chunk names exactly one code generation and file occurrence;
  symbol grains also name the generation-local symbol occurrence. Mixed-
  generation manifests or receipts are rejected before publication.
- Golden fixtures cover all five grains, deterministic oversized-symbol
  splitting, file-range coverage, unsupported/excluded ranges, and exact-term
  classification for a qualified symbol, compiler error, runtime error, CLI
  flag, tool name, and configuration key.
- No-op fixtures make zero projection calls. A one-symbol edit reports only
  that symbol's changed signature/body/member chunks, affected parent/file
  chunks, and deletions; every unrelated chunk remains in `reused` with the
  same ID and digest. A model-only projection-key change replays all eligible
  chunks without invoking parser or extractor fixtures.
- Receipt conformance rejects missing, duplicate, extra, reordered-without-
  canonicalization, wrong-request, wrong-prior/current-generation,
  wrong-digest, and wrong-key receipts; cancellation or failure activates no
  projection generation.
- Split-adapter tests keep canonical chunks, lexical postings, graph evidence,
  and projection receipts in separate in-memory stores and return the same
  identities and manifests, proving no table or embedding runtime is authority.
- One-file edits reparse only changed sanitized content/descriptor inputs and
  recompute only evidence-invalidated relation/attribution closures; reports
  separate parse work, resolution work, invalidation fan-out, reuse, and any
  conservative full rebuild without changing unchanged occurrence identities.
- A burst containing duplicate create/modify/save events produces the same
  manifest as one clean reconciliation. Rename pairs are stitched when the
  platform supplies stable file identity, and watcher overflow falls back to
  bounded `gix` reconciliation rather than a full rebuild or guessed deletion.
- Two linked worktrees sharing unchanged blobs may reuse physical parse/chunk
  artifacts, but publish different snapshot/generation/occurrence identities.
  An edit in one worktree invalidates no generation or cache entry in the
  other.
- Rename, move, split, merge, ambiguous-lineage, parse-error, deletion, and unsupported-language fixtures remain truthful.
- Fixtures prove Tree-sitter reuse never becomes lineage, parse/extraction caps
  remain partial, every graph path preserves its weakest edge authority and
  coverage, and unresolved dispatch cannot become semantic fact.
- Diagnostic and test attribution never crosses snapshots, never upgrades
  inference to fact, and never publishes stale or cleared evidence as current.
- Working, staged, and committed-range fixtures prove native Git hunk identity
  maps deterministically to generation-matched symbols while mismatch, binary,
  rename, deletion, ambiguous lineage, and missing-generation cases remain
  explicit. Caller, hazard, and affected-test results retain their own graph and
  test-evidence provenance rather than inheriting certainty from the Git hunk.
- TaskId-linked composition fixtures resolve exact generation/
  occurrence evidence through Plan 13 anchors, remain losslessly expandable,
  and introduce no task-owned rows or task authority into the code index; they
  are not a completion gate for this plan.
- Canonical descriptor fixtures prove analyzer routing and host LSP projection
  use the same extension, language-ID, root-marker, and capability facts without
  copying executable commands or settings into this boundary.
- Unsaved per-client LSP overlay fixtures create no durable generation rows.
  Sanitized dirty-worktree snapshots remain eligible for their own bounded
  durable generation, and matching saved content preserves producer provenance
  through capture and publication.
- Crash, cancellation, disk-full, stale-snapshot, and concurrent-build tests publish either one complete generation or none.
- Native historical-source fixtures ingest through sanitized logical batches
  with no indexer database open and no lost or duplicate supported records.
- Boundary regressions construct the indexer only through its validated intake
  and projection interfaces and reject filesystem, database, model-runtime,
  and transport authority in the indexing boundary; together they enforce
  capture as the only intake and store/projector composition as the only
  publication path.
- Focused non-indexing package checks do not compile Tree-sitter grammars or
  structural-search implementation, and this plan retains the production compilation
  measurements used by end-to-end performance comparison.
- Exact and lexical lanes are independently disableable and inspectable, use
  one immutable generation, and emit Plan 15 `CompactCandidate` values without a
  code-specific ranking kernel. One hundred shuffled producer/completion runs
  produce byte-identical IDs, order, contributions, explanations, coverage,
  and cursors.
- Every eligible exact identifier, qualified name, path, quoted phrase,
  compiler/runtime error, CLI flag, tool name, configuration key, and commit
  identifier precedes every approximate-only result. Exact admission
  precision, false promotion, and protected-stratum support are reported.
- The exact+lexical+graph baseline is versioned with its profile and
  fallback-subpayload bytes. Completion depends on validated callable behavior,
  direct tests, a passing Linux evaluation, and normal CI, not a historical
  artifact filename, saved-candidate packet, evidence map, or acceptance
  snapshot. A failed or pending result does not complete this delivery.
- Working/staged/range Git, history, blame, rename, binary, merge, and
  `HunkRef` fixtures retain native Git identity and independent Git/code/
  diagnostic/test watermarks. No code-index row becomes Git object or patch
  authority.
