# TraceDecay V2 Code Intelligence Indexing Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `tracedecay-code-index`, the deterministic code-intelligence indexing crate that owns language extraction (a versioned tree-sitter parser/grammar registry), watcher intake planning, incremental indexing with bounded dirty overlays, immutable packed snapshot/generation builds, symbol identity/lineage computation, and diagnostics/test-attribution mapping — the production side of the master plan's Code Intelligence bounded context (master §5.2 #6) whose write owner the master names in §7.7 ("Code indexer owns graph snapshots").

**Architecture:** Repository content enters exactly once through plan 03's `code_snapshot` extractor adapter, crosses the one mandatory Plan 18 sanitizer, and lands as immutable receipt-bound snapshot/file observations. The indexer library consumes only those sanitized observations: it parses receipt-bound file content with registered versioned grammars, derives symbol occurrences, code edges, diagnostics, and test attributions, and plans deterministic packed-generation builds. Plan 04's `code_evidence_v1` projector executes each build transactionally through a projector-owned port over plan 02's `GraphGenerationRepository`; plan 05 queries the published generations. One packed generation set plus bounded overlays and movable ref pointers replaces V1's physical database per branch.

**Tech Stack:** Rust workspace; `tracedecay-domain` contracts; `tree-sitter` 0.26-line runtime with bundled grammar crates pinned per release; deterministic canonical row encoding and SHA-256 digests; store `GenerationWriter`/manifest ports supplied by `tracedecay-store`; property, differential, copied-store, crash/disk-full, and Criterion tests.

Plan [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md) §8 requires federated selection by explicit repository/checkout/worktree/ref/snapshot/generation tuples; this crate produces the immutable generations those tuples name and never substitutes an active base checkout or currently published generation for a selected one.

---

## Goals

- Give the Code Intelligence context one production owner: extraction, incremental indexing, dirty-overlay computation, watcher intake planning, generation build planning, symbol identity/lineage, and diagnostics/test attribution live in this crate and nowhere else.
- Parse only receipt-bound sanitized file content delivered through plan 03's `code_snapshot` adapter; raw repository bytes, unkeyed content checksums, and unsanitized drafts never enter this crate (the locked pipeline: repo content → capture sanitizer → indexer → store generations → query).
- Make every generation build deterministic: the same sanitized source rows, grammar set, extractor set, resolver version, and build plan produce byte-identical canonical rows and the same generation content digest on every machine.
- Hold the observed scale envelope with headroom: 36k+ nodes, 71k+ edges, 978 files in the current TraceDecay graph, and the master §26 10× gate of one million symbols in a large project.
- Replace V1's 14 locally tracked per-branch graph databases (commonly ~140–150 MB each) with packed immutable generations, bounded dirty overlays, and ref pointers, so near-identical branch state is stored once.
- Keep symbol identity stable across renames, moves, splits, and merges through evidence-bearing lineage candidates; prefer a visible uncertain relationship over a silent incorrect merge (master §6.6).
- Map compiler/LSP diagnostics and test definitions/runs to snapshot occurrences with explicit evidence classes; attribution without evidence remains a candidate, never a fact.
- Choose incremental reuse over full rebuild by declared policy, and force full rebuilds only for declared reasons (schema bump, identity-rule change, privacy invalidation, corruption quarantine).
- Migrate the V1 branch graph stores under plan 12's controller with per-entity dispositions and receipts; retire the V1 graph stack under plan 19's deletion waves.

## Non-goals

- No source discovery, framing, sanitization, or observation-journal writes; plan 03 owns the `code_snapshot` adapter and the one sanitizer.
- No canonical event creation, projector checkpointing, or read-model transactions; plan 04's `code_evidence_v1` remains the transactional executor and registry owner.
- No SQL connections, physical generation publication, compaction execution, or blob I/O; plan 02's `GraphGenerationRepository` owns physical storage and atomic swaps.
- No query parsing, ranking, federation planning, or transport rendering; plan 05 owns code queries and plan 16 owns federated scope behavior.
- No per-branch physical databases, no mutable published generation, and no in-place historical rewrite.
- No secret detection or redaction; extraction consumes sanitizer output and can only propagate or narrow eligibility, never widen it.
- No LLM calls, network access, or ambient CWD/branch resolution anywhere in the crate.

## Convergence boundary

This crate is the sole extraction/indexing/generation-build owner in [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md); third-party language support plugs into its registry through the plan 19 §7.2 code extractor/grammar SPI (isolated-subprocess tier for native runtimes, per 19 §7.3). It consumes domain contracts from [`01-domain-crate.md`](01-domain-crate.md), sanitized observations produced by [`03-capture-crate.md`](03-capture-crate.md), and store ports from [`02-store-crate.md`](02-store-crate.md) ("Graph generation store"); it produces builds executed inside [`04-projectors-crate.md`](04-projectors-crate.md) PR 18 and queried by [`05-query-crate.md`](05-query-crate.md) §11.4.

Adding this crate satisfies the plan 19 §6.1 new-crate criteria explicitly: two real consumers (projectors' `code_evidence_v1` build port and root daemon composition's intake planner), a coherent bounded-context boundary (Code Intelligence production side), a dependency direction that only points at `tracedecay-domain`, the public contract and non-goals in this plan, independent tests/benchmarks, and the PR 33G/37C deletion/migration path for the V1 code it replaces. The workspace/dependency-DAG listing gains `tracedecay-code-index` in the plan 19 C0/C1 inventory slices.

| Boundary | Contract |
|---|---|
| Enters | Receipt-bound sanitized snapshot/file observations, registered grammar/extractor descriptors, explicit `ScopeSelectorV2` intake events, committed generation manifests, and identity-allocation results. |
| Exits | Deterministic extraction outputs, reuse/build plans, canonical generation rows and digests, overlay/compaction plans, symbol-entity proposals and lineage candidates, diagnostic/test attributions, and per-language coverage. |
| Upstream owner | Domain owns types and legal relations; capture owns ingress/sanitization; Plan 18 owns security invariants; store owns physical generations. |
| Downstream owner | Projectors alone commit rows/relations/generations; query alone plans/ranks/federates; no consumer re-parses repository content outside this crate. |
| Extension seam | A language adds a grammar descriptor, extraction query pack, extractor descriptor, redacted conformance fixtures, and determinism proof; it cannot add its own identity rules, generation schema, or sanitizer. |
| Scale/concurrency | Per-repository intake lanes, bounded parse budgets, per-file reuse, bounded overlay depth, streaming canonical row emission, and cancellation checkpoints; no cross-repository global lock. |
| Migration/retirement | V1 extractors/graph DBs are read-only parity fixtures and migration sources. After PR 33G receipts and plan 12 §15 PR 37C, the V1 extraction/branch-store stack is deleted under plan 19 §12.3 deletion waves. |

## Cross-crate contract

### Consumes

- `tracedecay-domain`: `EntityRef`, `CodeSnapshotId`, `GraphGenerationId`, symbol identity/occurrence/diagnostic/test entity kinds, code lineage/change/impact predicates, `RelationAssertionV1`, `ScopeSelectorV2`/`ScopeResolutionV2`, `VectorWatermark`, `CoverageReportV1`, sensitivity/retention classes, and sink-eligible text wrappers.
- Plan 03 outputs: sanitized `code.snapshot_observed`/`code.file_observed` observation families from the `code_snapshot` adapter, each binding one complete `SanitizationReceiptV1` (minted by capture, persisted per shard in plan 02's `sanitization_receipts` table), plus plan 03's fixed quarantine vocabulary (including `ownership_conflict` and `identity_collision`).
- Plan 02 ports, indirectly through the projector-owned build port: `GenerationWriter` staging, manifest verification, `open_resolved_snapshot`, and the storage ADR pack/overlay constants ("Graph generation store": 32/64/128 snapshots per pack, 512 MiB/1 GiB/2 GiB targets, overlay depth 2/4/8).
- Identity ledger results: symbol entities without an exact native key allocate through the store's `AllocationRequest` path exactly as plan 01 specifies; this crate proposes, it never mints UUIDs.

### Produces

- `ExtractionOutput` rows (occurrences, edges, annotations, per-file coverage) derived from receipt-bound content with descendant lineage intact.
- `GenerationBuildPlanV1`, canonical ordered generation rows, `GenerationDigest`, overlay plans, and compaction plans consumed by plan 04 PR 18 through `GenerationBuilderPort`.
- Symbol-entity proposals, `LineageCandidateV1` evidence sets, `DiagnosticAttributionV1`, and `TestAttributionV1` rows for projection as entities/relations.
- Intake decisions (capture-snapshot triggers with explicit dirty sets) consumed by root daemon composition; the daemon schedules plan 03 capture runs, this crate never invokes providers or the journal.
- No canonical event, no store row, no query result, no transport payload.

The dependency boundary is `tracedecay-domain <- tracedecay-code-index`, with `tracedecay-projectors -> tracedecay-code-index` and root composition `-> tracedecay-code-index`. Root-composition companion glue is `src/v2_adapters/code_index.rs`: it implements `GenerationBuilderPort` over store generation ports for the projector, and wires the intake planner to the daemon watcher. Neither capture nor query imports this crate; the adapter adds no extraction, identity, or policy semantics.

## Exact crate and module layout

| File | Responsibility |
|---|---|
| `crates/tracedecay-code-index/Cargo.toml` | Crate dependencies, grammar-tier features mirroring V1's `default`/`medium`/`full` sets; no network feature. |
| `crates/tracedecay-code-index/src/lib.rs` | Public exports only. |
| `crates/tracedecay-code-index/src/error.rs` | Typed grammar, parse, budget, identity, reuse, build, digest, and migration errors; log-safe fields only. |
| `crates/tracedecay-code-index/src/grammar.rs` | `GrammarDescriptorV1`, ABI checks, pinned grammar-crate versions, query-pack digests, tier gating. |
| `crates/tracedecay-code-index/src/registry.rs` | `ExtractorRegistryV1`: unique language ownership, grammar/ABI compatibility, plugin-tier validation, registry digest. |
| `crates/tracedecay-code-index/src/extract/mod.rs` | `LanguageExtractor` contract, budgets, and shared extraction driver. |
| `crates/tracedecay-code-index/src/extract/driver.rs` | Bounded tree-sitter parse, error-range recovery, redaction-marker handling, cancellation checkpoints. |
| `crates/tracedecay-code-index/src/extract/queries.rs` | Versioned extraction query packs per language; declarative capture-to-draft lowering. |
| `crates/tracedecay-code-index/src/extract/langs/` | Per-language extractor specs superseding V1's ~60 `src/extraction/*_extractor.rs` modules; one file per language family. |
| `crates/tracedecay-code-index/src/identity.rs` | `SymbolIdentitySeedV1`, occurrence-ID derivation, disambiguators, collision handling. |
| `crates/tracedecay-code-index/src/intake.rs` | Watcher intake planner: debounce, coalescing, per-repository lanes, storm rejection, explicit-scope preservation. |
| `crates/tracedecay-code-index/src/incremental.rs` | `ReusePlanV1`: per-file reuse keys, language-scoped invalidation, resolver-only refresh, full-rebuild reasons. |
| `crates/tracedecay-code-index/src/overlay.rs` | Bounded dirty-overlay computation, depth/ratio thresholds, compaction eligibility. |
| `crates/tracedecay-code-index/src/build/mod.rs` | `GenerationBuildPlanV1`, `GenerationBuilderPort`, staged-build orchestration under the projector transaction. |
| `crates/tracedecay-code-index/src/build/rows.rs` | Canonical row types and total ordering for every generation table. |
| `crates/tracedecay-code-index/src/build/digest.rs` | Streaming canonical-row digest; digest excludes compression and physical layout. |
| `crates/tracedecay-code-index/src/resolve.rs` | Edge resolution (call/type/use/import/impl/annotation), resolver versioning, unresolved-target retention. |
| `crates/tracedecay-code-index/src/lineage.rs` | `LineageCandidateV1` computation: rename/move/split/merge evidence and confidence. |
| `crates/tracedecay-code-index/src/diagnostics.rs` | `DiagnosticAttributionV1`: diagnostic-to-occurrence mapping with evidence and coverage. |
| `crates/tracedecay-code-index/src/test_attribution.rs` | `TestAttributionV1`: static candidate mapping plus recorded-run exact evidence. |
| `crates/tracedecay-code-index/src/migrate_v1.rs` | V1 branch-graph-store readers as parity fixtures and disposition inputs for PR 33G. |
| `crates/tracedecay-code-index/tests/extraction_conformance.rs` | Per-language redacted golden fixtures, determinism, error-range and redaction-marker behavior. |
| `crates/tracedecay-code-index/tests/incremental_suite.rs` | Reuse keys, invalidation matrix, overlay bounds, intake debounce/storm cases. |
| `crates/tracedecay-code-index/tests/generation_suite.rs` | Canonical ordering, digest determinism, plan/port contracts, schema conformance. |
| `crates/tracedecay-code-index/tests/lineage_attribution_suite.rs` | Rename/move/split/merge cases, diagnostic/test mapping evidence classes. |
| `crates/tracedecay-code-index/tests/migration_parity.rs` | Copied V1 branch-store differential parity, disposition manifests, disk math. |
| `crates/tracedecay-code-index/benches/code_index.rs` | Parse/extract throughput, incremental latency, generation build, digest, 10× symbol scale. |

## Public API and fixed signatures

```rust
pub struct GrammarDescriptorV1 {
    pub grammar_id: &'static str,
    pub grammar_crate: &'static str,
    pub grammar_crate_version: &'static str,
    pub abi_version: u32,
    pub tier: GrammarTier,
    pub query_pack_digest: [u8; 32],
}

pub enum GrammarTier { Default, Medium, Full, Plugin }

pub struct ExtractorDescriptorV1 {
    pub extractor_id: &'static str,
    pub extractor_version: &'static str,
    pub grammar_id: &'static str,
    pub languages: &'static [LanguageId],
    pub capabilities: ExtractorCapabilities,
}

pub trait LanguageExtractor: Send + Sync {
    fn descriptor(&self) -> &'static ExtractorDescriptorV1;
    fn extract(
        &self,
        unit: &ExtractionUnit<'_>,
        budget: ExtractionBudget,
    ) -> Result<ExtractionOutput, CodeIndexError>;
}

pub struct ExtractionUnit<'a> {
    pub snapshot: CodeSnapshotId,
    pub file: EntityRef,
    pub language: LanguageId,
    pub content: &'a SearchEligibleText,
    pub receipt: SanitizationReceiptId,
    pub redaction_markers: &'a [RedactionMarkerSpan],
}

pub struct ExtractionOutput {
    pub occurrences: Vec<SymbolOccurrenceDraft>,
    pub edges: Vec<CodeEdgeDraft>,
    pub annotations: Vec<AnnotationDraft>,
    pub coverage: ExtractionCoverage,
}

pub enum ExtractionCoverage {
    Parsed,
    Partial { error_ranges: Vec<ByteRange> },
    RedactedStructural { redacted_ranges: u32 },
    RedactedOpaque,
    Unsupported { reason: UnsupportedReason },
    BudgetExceeded { parsed_bytes: u64 },
}
```

```rust
pub struct ExtractorRegistryV1;

impl ExtractorRegistryV1 {
    pub fn builtin() -> Result<Self, CodeIndexError>;
    pub fn register(&mut self, extractor: Box<dyn LanguageExtractor>) -> Result<(), CodeIndexError>;
    pub fn validate(&self) -> Result<RegistryReport, CodeIndexError>;
    pub fn registry_digest(&self) -> ExtractorSetDigest;
}

pub struct SymbolIdentitySeedV1 {
    pub repository: EntityRef,
    pub language: LanguageId,
    pub qualified_path: QualifiedNamePath,
    pub kind: SymbolKind,
    pub disambiguator: SymbolDisambiguator,
}

pub fn propose_symbol_entity(seed: &SymbolIdentitySeedV1) -> SymbolEntityProposal;
pub fn derive_occurrence_id(
    snapshot: &CodeSnapshotId,
    file: &EntityRef,
    range: ByteRange,
    kind: SymbolKind,
) -> OccurrenceId;
```

```rust
pub struct GenerationBuildPlanV1 {
    pub repository: EntityRef,
    pub privacy_domain: PrivacyDomainId,
    pub snapshots: Vec<CodeSnapshotId>,
    pub inputs: BuildInputDigests,
    pub reuse: ReusePlanV1,
    pub target: BuildTarget,
}

pub struct BuildInputDigests {
    pub extractor_set: ExtractorSetDigest,
    pub grammar_set: [u8; 32],
    pub resolver_version: ResolverVersion,
    pub generation_schema_version: u32,
    pub source_watermark: VectorWatermark,
}

pub enum BuildTarget {
    NewPack,
    Overlay { base: GraphGenerationId },
    Compaction { merge: Vec<GraphGenerationId> },
}

pub trait GenerationBuilderPort: Send + Sync {
    fn stage(&self, plan: &GenerationBuildPlanV1) -> Result<StagedGenerationHandle, CodeIndexError>;
    fn emit_rows(
        &self,
        handle: &mut StagedGenerationHandle,
        rows: CanonicalRowBatch,
    ) -> Result<(), CodeIndexError>;
    fn seal(
        &self,
        handle: StagedGenerationHandle,
        digest: GenerationDigest,
    ) -> Result<SealedGenerationReceipt, CodeIndexError>;
}

pub struct GenerationDigest(pub [u8; 32]);
```

- `ExtractionUnit.content` is the only content input and is receipt-bound sanitized text; the crate has no constructor for content from paths, readers, or raw bytes. Redaction markers are explicit spans so parse recovery can keep structural identity for redacted files (plan 18 §11.2); a file whose markers break parsing degrades to `RedactedStructural`/`RedactedOpaque` coverage, never to a raw-content retry.
- `ExtractorRegistryV1::validate` fails on duplicate language ownership, ABI mismatch with the pinned tree-sitter runtime, missing query-pack digest, or a plugin-tier extractor without the plan 19 §7.3 isolated-subprocess declaration.
- `GenerationBuilderPort` is implemented over plan 02's `GenerationWriter` by root composition; `seal` fails when the store-verified row manifest disagrees with `GenerationDigest`. This crate never opens a database and never calls `publish_generation`; publication/atomic swap remain plan 02's sequence executed under plan 04's PR 18 transaction.
- Ordering: plan 04's PR 18 wires `code_evidence_v1` to this port; PR 18B–18D below land first so PR 18 consumes a real builder, with an in-crate fixture builder available for earlier projector framework tests.

### Consumed observation families and derived-row lineage

The `code_snapshot` adapter (plan 03) commits these sanitized observation payload kinds; they are the crate's only content inputs and the payload families plan 04's registry must show owned by `code_evidence_v1`:

- `code.snapshot_observed` — repository/checkout/worktree/ref tuple, snapshot kind (commit or dirty overlay base), source watermark.
- `code.file_observed` — file path, language hint, keyed content fingerprint, receipt-bound sanitized content reference, redaction-marker spans.
- `code.file_removed` — deletion evidence for incremental planning.
- `code.diagnostic_observed` / `code.build_observed` / `code.test_run_observed` — tool outputs framed by capture from compiler/LSP/test processes.

Every row this crate derives (occurrence, edge, diagnostic mapping, test attribution, FTS/fingerprint auxiliary) records the observation IDs and receipt IDs it descends from. Receipt revocation or a retroactive privacy finding (plan 18 §12) invalidates descendants through plan 04 PR 10A's lineage mechanism and forces `PrivacyPolicyInvalidation` rebuilds scoped to the affected files — never a whole-profile rebuild by default.

### Deterministic identity, canonical rows, and generation digests

- A symbol entity's seed identity is `(repository, language, qualified_path, kind, disambiguator)`; the disambiguator covers overload arity/signature and same-name siblings. Seeds propose; the store's identity ledger allocates (plan 01's `AllocationRequest` path for entities lacking an exact native key). A seed collision with a live different-lineage entity is surfaced as an `identity_collision` conflict, never silently reused — the regression class behind PR #269/#371 in plan 14 §2 (its `FM-###` row IDs bind PR 33G/18F receipts).
- Occurrence identity is `(snapshot, file, byte range, kind)` — deterministic, snapshot-scoped, and independent of allocation order.
- Canonical row ordering is total and fixed: files by path bytes, occurrences by `(file, byte_start, kind, qualified_path)`, edges by `(source_occurrence, kind, target)`, diagnostics by `(file, byte_start, tool, code)`, test-map rows by `(test, covered_entity)`. The generation digest is computed over the canonical uncompressed ordered rows plus `BuildInputDigests`; compression, page layout, and physical file boundaries are excluded.
- Same sanitized source rows + same `BuildInputDigests` ⇒ same `GenerationDigest`, on any machine, in any parallelism configuration. Two consecutive builds asserting digest equality is a release gate.
- Content fingerprints stored in generation tables are privacy-domain-keyed fingerprints carried from capture; no unkeyed hash of file content is computed or stored in this crate (the master's sanitize-before-persist and keyed-fingerprint invariant).

### Watcher intake and incremental indexing

```rust
pub struct IndexIntakeEventV1 {
    pub scope: ScopeSelectorV2,
    pub kind: IntakeKind,
    pub observed_at: UtcMicros,
}

pub enum IntakeKind {
    CommitDetected { checkout: EntityRef, commit: EntityRef },
    RefMoved { checkout: EntityRef, reference: EntityRef },
    WorktreeDirty { worktree: EntityRef, paths: Vec<ClassifiedLocator> },
    ManualRefresh,
    GrammarOrExtractorUpgrade { languages: Vec<LanguageId> },
}

pub struct IntakePlanner;

impl IntakePlanner {
    pub fn decide(
        &self,
        events: &[IndexIntakeEventV1],
        state: &IntakeLaneState,
    ) -> IntakeDecision;
}

pub enum IntakeDecision {
    CaptureSnapshot { scope: ScopeSelectorV2, dirty_set: DirtySet },
    Coalesce { until: UtcMicros },
    Defer { reason: DeferReason, until: UtcMicros },
    RejectStorm { window: UtcMicros, dropped_events: u64 },
}
```

- The daemon watcher (root composition; V1 seam `src/daemon/git_watch.rs`, source-framed by plan 03's `adapters/git.rs`) feeds intake events; the planner debounces (default 500 ms), coalesces per `(repository, checkout, worktree)` lane, and rejects event storms with a visible marker instead of unbounded queueing.
- Every `CaptureSnapshot` decision preserves the explicit `ScopeSelectorV2`; the planner never substitutes CWD, active base checkout, or current branch, and an empty selector is rejected exactly as in plan 03.
```rust
pub struct ReusePlanV1 {
    pub reused_files: Vec<FileReuseRef>,
    pub reparse_files: Vec<EntityRef>,
    pub reresolve_only: bool,
    pub full_rebuild: Option<FullRebuildReason>,
}

pub struct FileReuseRef {
    pub file: EntityRef,
    pub reuse_key: FileReuseKey,
    pub source_generation: GraphGenerationId,
}

pub struct FileReuseKey {
    pub content_fingerprint: [u8; 32], // privacy-domain-keyed, carried from capture
    pub grammar_crate_version: &'static str,
    pub extractor_version: &'static str,
    pub query_pack_digest: [u8; 32],
}

pub enum FullRebuildReason {
    GenerationSchemaBump,
    IdentityRuleChange,
    PrivacyPolicyInvalidation,
    CorruptionQuarantine,
}

pub struct DirtySet {
    pub files: Vec<ClassifiedLocator>,
    pub deleted: Vec<ClassifiedLocator>,
    pub truncated: bool, // storm/limit truncation is visible, never silent
}
```

- Incremental reuse keys are `(keyed content fingerprint, grammar_crate_version, extractor_version, query_pack_digest)` per file. Unchanged keys reuse prior occurrences/edges; changed grammar/extractor versions invalidate only their languages; a resolver-version bump re-runs edge resolution from retained occurrences without re-parsing.
- `FullRebuildReason` is a closed enum: `GenerationSchemaBump`, `IdentityRuleChange`, `PrivacyPolicyInvalidation` (receipt revocation/descendant invalidation per plan 04 PR 10A), `CorruptionQuarantine`. Anything else must be expressible as bounded reuse; "rebuild everything to be safe" is not a legal decision.
- Dirty worktree changes build bounded overlays over the base snapshot generation. Overlay depth and overlay/base row-ratio bounds come from plan 02's storage ADR (depth candidates 2/4/8); exceeding either marks the lane compaction-eligible. Compaction is planned here, executed by plan 02's `compact`.

### Symbol lineage

```rust
pub struct LineageCandidateV1 {
    pub from: EntityRef,
    pub to_seed: SymbolIdentitySeedV1,
    pub kind: LineageKind,
    pub evidence: Vec<LineageEvidence>,
    pub confidence: Confidence,
}

pub enum LineageKind { Rename, Move, Split, Merge, SameLineage }

pub enum LineageEvidence {
    GitRenameDetection { similarity_bp: u16 },
    BodyFingerprintMatch,
    SignatureMatch,
    ReferenceMajority { moved_refs: u32, total_refs: u32 },
    ContainerMove,
}
```

- Lineage candidates are computed across consecutive snapshots from keyed body fingerprints, signatures, container paths, Git rename detection, and reference-majority evidence. They project as plan 01 code-lineage `RelationAssertionV1` rows (evidence class `derived-exact` only for exact-fingerprint moves; otherwise `inferred` with confidence/rationale).
- Ambiguous lineage remains a candidate set; the crate never auto-merges entities. Merge/split materialization is a projector decision under registry predicate rules, and the identity ledger records aliases so old `SymbolEntity` references keep resolving.
- The labeled lineage corpus and its F1 ≥ 98% gate are shared with plan 04's release gates; this crate owns the candidate generator that the gate measures.

### Diagnostics and test attribution

```rust
pub struct DiagnosticAttributionV1 {
    pub diagnostic: EntityRef,
    pub tool: DiagnosticTool,
    pub tool_version: String,
    pub snapshot: CodeSnapshotId,
    pub file: EntityRef,
    pub range: ByteRange,
    pub mapped_occurrence: Option<OccurrenceId>,
    pub mapping: MappingEvidence,
}

pub struct TestAttributionV1 {
    pub test_occurrence: OccurrenceId,
    pub covered_entity: EntityRef,
    pub evidence_class: AttributionEvidenceClass,
    pub run_ref: Option<EventId>,
    pub confidence: Confidence,
}

pub enum AttributionEvidenceClass { RecordedRunExact, StaticInferred }
```

- Diagnostics arrive as sanitized observations (V1 seams `src/diagnostics/{rust,python,typescript}.rs` and `src/diagnostics/lsp`); mapping to occurrences uses file + range containment within the exact diagnosed snapshot. A diagnostic against a snapshot this index has not built maps to explicit `snapshot_not_indexed` coverage, never to the nearest current occurrence.
- Test attribution has two evidence classes only: `RecordedRunExact` (a captured test-run event names the test and the executed code, e.g. nextest/libtest output observed through capture) and `StaticInferred` (import/call-edge reachability). Ratios and "affected tests" answers in plan 05 must be able to filter by class; this crate never blends them into one score.
- V1 parity targets: `tracedecay_test_map`/`tracedecay_run_affected_tests` behavior (V1 `src/tool_command.rs`) and `src/graph/health/test_risk.rs` outputs become differential fixtures in PR 18F.

## Packed generation and overlay schema

Plan 02 owns the physical `GraphGenerationRepository`, pack/overlay ADR constants, publication, and manifest mapping. This plan owns the generation-internal logical schema (plan 02's schema-ownership rule applies: these column-level schemas land in the owning implementation PR — PR 18D — before code). All tables live inside packed generation files in the project/privacy-domain graph store; every content-bearing row binds a sanitization receipt ID resolvable in the owning shard's `sanitization_receipts` table (plan 02's schema, minted only by plan 03's sanitizer).

| Table | Schema (fields, PK, uniqueness, indexes, retention/size) |
|---|---|
| `generation_manifest` | `generation_id TEXT PK (UUIDv7)`, `repository_entity TEXT NOT NULL`, `privacy_domain TEXT NOT NULL`, `generation_schema_version INTEGER NOT NULL`, `extractor_set_digest BLOB(32) NOT NULL`, `grammar_set_digest BLOB(32) NOT NULL`, `resolver_version TEXT NOT NULL`, `build_plan_digest BLOB(32) NOT NULL`, `content_digest BLOB(32) NOT NULL`, `snapshot_count INTEGER`, `file_count INTEGER`, `symbol_count INTEGER`, `edge_count INTEGER`, `source_watermark BLOB NOT NULL`, `built_at INTEGER NOT NULL`. UNIQUE `(repository_entity, content_digest)`. Index `(repository_entity, built_at)`. One row per generation; retained while the generation is referenced by plan 02's manifest or rollback window. |
| `gen_snapshots` | `snapshot_id TEXT PK`, `kind TEXT CHECK (kind IN ('commit','dirty_overlay')) NOT NULL`, `base_snapshot_id TEXT NULL`, `commit_entity TEXT NULL`, `worktree_entity TEXT NULL`, `source_watermark BLOB NOT NULL`. Index `(base_snapshot_id)`. Bounded by pack size (32/64/128 snapshots per ADR candidate). |
| `gen_file_content` | `content_key BLOB(32) PK` (privacy-domain-keyed fingerprint), `bytes BLOB NOT NULL` (zstd, fixed params), `original_len INTEGER NOT NULL`, `sanitization_receipt_id TEXT NOT NULL`. Deduplicated across snapshots within the generation; dominant size contributor, sized by the pack-target ADR (512 MiB–2 GiB candidates). |
| `gen_files` | `(snapshot_id, file_id) PK`, `path TEXT NOT NULL`, `language TEXT NOT NULL`, `content_key BLOB(32) NOT NULL REFERENCES gen_file_content`, `size_bytes INTEGER NOT NULL`, `extractor_version TEXT NOT NULL`, `coverage TEXT NOT NULL`. UNIQUE `(snapshot_id, path)`. Indexes `(content_key)`, `(language)`. ~978 rows/snapshot currently; 10× gate rows stay cursor-paged. |
| `gen_symbol_occurrences` | `(snapshot_id, occurrence_id) PK`, `symbol_entity TEXT NOT NULL`, `file_id TEXT NOT NULL`, `byte_start INTEGER NOT NULL`, `byte_end INTEGER NOT NULL`, `kind TEXT NOT NULL`, `qualified_name TEXT NOT NULL`, `signature TEXT NULL`, `visibility TEXT NULL`, `extractor_version TEXT NOT NULL`, `sanitization_receipt_id TEXT NOT NULL`. UNIQUE `(snapshot_id, file_id, byte_start, kind)`. Indexes `(symbol_entity, snapshot_id)`, `(file_id)`. Current scale 36k+/branch; 1M-symbol 10× gate. |
| `gen_edges` | `(snapshot_id, edge_id) PK`, `source_occurrence TEXT NOT NULL`, `target_occurrence TEXT NULL`, `target_symbol_entity TEXT NULL`, `kind TEXT NOT NULL`, `byte_start INTEGER`, `byte_end INTEGER`, `resolver_version TEXT NOT NULL`, `confidence INTEGER NULL`. CHECK: exactly one of `target_occurrence`/`target_symbol_entity`/unresolved marker. Indexes `(source_occurrence)`, `(target_symbol_entity, kind)`. Current scale 71k+/branch. |
| `gen_diagnostics` | `(snapshot_id, diagnostic_id) PK`, `file_id`, `byte_start`, `byte_end`, `severity TEXT`, `tool TEXT`, `tool_version TEXT`, `code TEXT NULL`, `mapped_occurrence TEXT NULL`, `mapping_evidence TEXT NOT NULL`, `sanitization_receipt_id TEXT NOT NULL`. Indexes `(file_id)`, `(mapped_occurrence)`. Retention follows the generation. |
| `gen_tests` / `gen_test_map` | `gen_tests`: `(snapshot_id, test_occurrence) PK`, `framework TEXT`, `test_name TEXT NOT NULL`. `gen_test_map`: `(snapshot_id, test_occurrence, covered_entity) PK`, `evidence_class TEXT CHECK (evidence_class IN ('recorded_run_exact','static_inferred'))`, `run_ref TEXT NULL`, `confidence INTEGER NULL`. Reverse index `(covered_entity)`. |
| `overlay_journal` / `overlay_files` | `overlay_journal`: `overlay_id TEXT PK`, `base_snapshot_id TEXT NOT NULL`, `worktree_entity TEXT NOT NULL`, `depth INTEGER NOT NULL`, `created_at INTEGER`. `overlay_files`: `(overlay_id, file_id) PK`, `change_kind TEXT CHECK (change_kind IN ('added','modified','deleted'))`, `content_key BLOB(32) NULL`. Index `(base_snapshot_id)`. Retention: overlays are pruned at compaction; depth bounded by the ADR constant. |
| `gen_fts`, `gen_fingerprints`, `gen_complexity`, `gen_redundancy` | Rebuildable auxiliary families keyed by `(snapshot_id, occurrence_id | file_id)` with the extractor/resolver version that produced them. Per plan 02's rule for remaining table families: one-line key+index envelope here; column-level schema must land in the owning implementation PR (18D/18F) before code. FTS indexes only receipt-bound `SearchEligibleText`. |

## Scale envelope and physical policy

- Current observed scale: 36k+ nodes, 71k+ edges, 978 files (master §2.1); 14 locally tracked per-branch V1 graph stores at ~140–150 MB each ≈ 2+ GB of largely duplicated state. V2 target at current scale: one packed generation set plus overlays for the same branch coverage at ≤ 1.2× the largest single V1 branch store, verified in PR 33G's disk-math receipt against the master §26 2.25× migration-amplification gate.
- Master §26 limits honored by construction: ≤ 10,000 graph generation/overlay files per profile after compaction, ≤ 8 generation files open per query process (plan 02), WAL ≤ 1 GB per shard before checkpoint, and the 10× corpus (1M symbols in a large project) benchmarked in `benches/code_index.rs` with recorded reference machine and peak RSS.
- Refs are movable pointers to immutable snapshots (plan 02 manifest mapping); a ref move is a manifest update, never a rebuild, and never mutates a prior snapshot binding (plan 04 PR 18's `ref_move_does_not_mutate_old_snapshot_binding` is the shared contract test).
- Extraction and build are streaming: canonical rows are emitted in bounded batches; peak RSS during a full current-scale build is recorded and must not scale with total corpus size beyond declared per-batch bounds.

## V1 seam map and ownership

| V1 seam | V2 owner | Result |
|---|---|---|
| `src/extraction/*_extractor.rs` (~60 per-language tree-sitter extractors), `src/extraction/{common,basic_common,batch_extractor,annotations,complexity}.rs` | `src/extract/**`, `src/registry.rs` | Declarative query packs + shared driver replace per-language imperative extractors; V1 outputs become differential fixtures; unknown-language behavior becomes explicit `Unsupported` coverage. |
| `src/extraction_worker.rs`, `src/sync.rs` (read/hash/change detection) | `src/intake.rs`, `src/incremental.rs`, plan 03 `code_snapshot` adapter | Ingest-side reading/hashing moves behind capture's sanitizer; reuse planning replaces rescan heuristics; UTF-16/BOM handling is adapter framing. |
| `src/db/{nodes,edges,files,fingerprints,coverage,search,unresolved,redundancy_pairs}.rs`, `src/db/migrations.rs` (`LATEST_VERSION`, currently 17) | Generation schema above + plan 02 "Graph generation store" | Mutable per-branch tables become immutable packed generation tables; V1 schema version and DB hash become import-manifest fields exactly as plan 01's migration seam table states. |
| `src/branch.rs`, `src/branch_meta.rs`, per-branch DB layout in `src/storage.rs` | Plan 02 manifests + ref pointers | A branch is a movable ref naming a snapshot/generation; no branch owns a database. Retirement under plan 12 §15 PR 37C. |
| `src/graph/{queries,traversal,scc}.rs`, `src/graph/health/**` (incl. `test_risk.rs`) | Plan 05 §11.4 operators over published generations | Query semantics move to the query crate; this crate supplies the data and the differential fixtures. |
| `src/diagnostics/{rust,python,typescript}.rs`, `src/diagnostics/lsp/**`, `src/diagnostics/{cache,fingerprint}.rs` | `src/diagnostics.rs` + capture diagnostic observations | Tool output is captured/sanitized once; mapping is deterministic against the diagnosed snapshot; caches become rebuildable derived state. |
| `src/daemon/git_watch.rs` index-trigger behavior | `src/intake.rs` hosted by root daemon (plan 12 §13) | Watcher events become typed intake events with debounce/coalesce/storm policy and explicit scope. |
| `src/redundancy.rs`, `src/ast_grep_search.rs` graph-build dependencies | Auxiliary generation families + plan 05 | Redundancy/fingerprint data are rebuildable generation rows; search surfaces route through the query crate. |

## Per-language conformance matrix

Every registered extractor ships redacted golden fixtures asserting at least the rows below; `adapters`-style registry validation fails on an untested registry entry, matching plan 03's per-provider discipline. Tier composition mirrors the V1 Cargo feature sets so packaged builds keep their current language surface.

| Language family | Required fixture assertions |
|---|---|
| Rust (default tier) | Modules/impl blocks/traits/generics; function/method/closure occurrences; call/type-use/impl edges; `#[test]`/`#[cfg(test)]` detection; macro-generated span coverage as `Partial`, never fabricated symbols; overload-free disambiguator stability. |
| TypeScript/JavaScript (default tier) | ESM/CJS imports as edges; classes/interfaces/enums; arrow/anonymous functions with deterministic disambiguators; JSX components; declaration merging as candidate lineage, not silent merge; `.d.ts` visibility. |
| Python (default tier) | Def/class/nested scopes; decorators as annotations; dynamic attribute access retained as unresolved edges; pytest/unittest test detection; indentation-error files as `Partial` with exact error ranges. |
| Go / Java / Kotlin / C# / C / C++ (default tier) | Package/namespace qualified paths; interface/implementation edges; header/source occurrence pairing (C/C++) without merging entities; test-framework detection per ecosystem. |
| Medium tier (Dart, Pascal, PHP, Ruby, Bash, Protobuf, PowerShell, Nix, VB.NET) | Symbol/edge families the V1 extractor produced, asserted against V1 differential goldens; unknown constructs degrade to coverage, not dropped rows. |
| Full tier (Lua, Zig, Obj-C, Perl, Fortran, COBOL, basics, Dockerfile, shader/markup/data languages, functional family, TOML, Lean) | Occurrence extraction and file-level structure; edge extraction only where the V1 extractor asserted it; per-language `Unsupported` reasons for constructs V1 also skipped. |
| Plugin tier | Registry validation of descriptor/ABI/query-pack digest; isolated-subprocess execution under plan 19 §7.3 budgets; determinism proof identical to built-ins; no plugin-supplied identity or schema. |
| Redaction cases (every tier) | One fixture per language with redaction markers inside string literals, comments, and identifiers; asserts structural identity retention rules and zero candidate bytes in any output row. |

All language fixtures assert canonical row bytes, coverage kind, extractor/grammar versions, and second-run byte identity.

Merged PR #405 (legacy-store adoption) governs repository identity for every V1 graph store this plan touches: moved roots, symlinks, and linked worktrees resolve to one adopted identity, and nonempty split identities quarantine as `ownership_conflict`/`identity_collision` rather than minting duplicate repository or symbol identities — the plan 14 §2 identity rows (PR #269/#371) are the binding regression class. PR #406's disk-full corruption row fixes the staging discipline: no build ever replaces the last good generation, and corrupt families quarantine with their recovery set preserved.

## Downstream query-surface handoff

The V1 graph tool surface survives as plan 05 query semantics over published generations; this crate guarantees the data exists with the fields those operators need. Dispositions here feed plan 21's inventory; none of these behaviors remains implemented against V1 stores after cutover.

| V1 behavior | Generation data this plan supplies | V2 query owner |
|---|---|---|
| Symbol lookup / outline / body ranges | `gen_symbol_occurrences` qualified names, kinds, ranges; `gen_file_content` receipt-bound text | Plan 05 §11.1 FTS + list intents |
| Callers/callees/call chains | `gen_edges` call edges with resolver version and unresolved-target retention | Plan 05 §11.4 graph operators |
| Impact/affected analysis | Edge closure inputs + snapshot tuples | Plan 05 §11.4; federation per plan 16 §8 |
| Test map / affected tests / test risk | `gen_tests`/`gen_test_map` with dual evidence classes | Plan 05 §11.4 + plan 04 read models |
| Diagnostics-to-symbol mapping | `gen_diagnostics.mapped_occurrence` with mapping evidence | Plan 05; Observatory views per plan 11 |
| Complexity/redundancy/health scans | Auxiliary generation families (`gen_complexity`, `gen_redundancy`, `gen_fingerprints`) | Plan 05 aggregates; plan 19 §13 scorecard inputs |
| Dead-code/unused-import scans | Occurrence/edge reachability rows | Plan 05 §11.4 bounded traversals |
| Cross-branch graph search/compare | Immutable generations named by ref pointers; both endpoint tuples on cross-generation joins | Plan 05 + plan 16 §8.2 |

## Migration from V1 branch graph stores

Coordinated with plan 12 (§14 controller phases, §15 retirement map row PR 37C) and consuming plan 12 PR 3R's inventory as the single source of the store list:

1. **Inventory:** enumerate every locally tracked V1 branch graph DB per repository from the PR 3R ledger with size, schema version (`LATEST_VERSION` lineage), branch ref, last-write time, and adoption identity from PR #405 manifests.
2. **Durable-data carve-out:** graph-resident durable rows (memory/fact tables inside `tracedecay.db`-era layouts) are migrated by the knowledge migration owner before any graph DB archive — plan 12 §15's PR 37C precondition; this plan never deletes a store containing unmigrated durable rows.
3. **Re-index-first policy:** where the branch's commit is still resolvable, V2 re-extracts deterministically from source at that snapshot and the V1 store is used only as a differential parity fixture; imported V1 rows are never trusted as canonical. Where the commit is unresolvable (orphaned branch, pruned objects), the V1 store's rows import as evidence-class `derived-exact` rows with `v1_import` provenance and the V1 DB hash in the import manifest.
4. **Dispositions:** every V1 store and every carved family receives exactly one plan 12 backfill-manifest disposition — `retained | skipped | quarantined | redacted | deleted` (plan 12's backfill-manifest vocabulary; plan 12 owns the schema) — and the receipt binds the relevant plan 14 `FM-###` row IDs for the #269/#371 identity and #406 corruption classes.
5. **Disk math:** the PR 33G receipt records before/after bytes (14 stores × ~140–150 MB vs packed generations + overlays) and proves the master §26 migration disk-amplification ≤ 2.25× gate.
6. **Retirement:** after parity receipts and one read-only release window, PR 37C deletes the V1 branch-store stack under plan 19 §12.3's deletion waves; the adapter-free end state is plan 19's convergence gate, not this plan's.

Import receipts are durable rows (G4), written by PR 33G in the owning project shard:

| Table | Schema |
|---|---|
| `v1_graph_import_receipts` | `receipt_id TEXT PK (UUIDv7)`, `v1_store_path_alias TEXT NOT NULL` (adopted-identity alias, not a raw path), `v1_db_hash BLOB(32) NOT NULL`, `v1_schema_version INTEGER NOT NULL`, `branch_ref_entity TEXT NULL`, `strategy TEXT CHECK (strategy IN ('reindexed','imported','dropped')) NOT NULL`, `disposition TEXT CHECK (disposition IN ('retained','skipped','quarantined','redacted','deleted')) NOT NULL`, `target_generation TEXT NULL`, `fm_row_ids TEXT NOT NULL` (bound plan 14 row IDs), `bytes_before INTEGER NOT NULL`, `bytes_after INTEGER NULL`, `signature BLOB NOT NULL` (HMAC, profile-local signing key per plan 12's receipt mechanism), `created_at INTEGER NOT NULL`. UNIQUE `(v1_db_hash)`. Index `(strategy, disposition)`. Retained for the full rollback/evidence window; never deleted with the store it describes. |

## Fault matrix

| Fault | Detection | Response | Gate |
|---|---|---|---|
| Disk full / kill during staged build | Store staging verification; startup scan (plan 02) | Last good generation untouched; staging removed; build resumes from plan checkpoints | #406-class kill test at every stage/emit/seal boundary |
| Grammar panic / pathological parse | Per-file parse budget, catch-unwind at the extractor boundary | File degrades to `BudgetExceeded`/`Partial` coverage; lane continues | No single file can block a repository build |
| Redaction markers break parsing | Driver error-range recovery | `RedactedStructural` else `RedactedOpaque` coverage; never a raw-content retry | Secret-corpus fixture per language |
| Identity seed collision | Ledger conflict on allocation | `identity_collision` quarantine + candidate relation; no silent reuse | #269/#371-class moved/linked-worktree fixtures |
| Overlay depth/ratio exceeded | Overlay planner thresholds | Compaction-eligible marker; bounded overlay refused beyond ADR depth | Overlay bound property test |
| Watcher event storm | Lane rate window | `RejectStorm` with dropped-event count visible in coverage | Storm fixture: 10k events/s, bounded memory |
| Stale/ambiguous scope at intake | `ScopeSelectorV2` resolution | Coverage/candidates per plan 01; never CWD/base fallback | Shared scope regression corpus (plan 16 §18) |
| Corrupt V1 store during migration | Reader checksum/open failure | `quarantined` disposition + receipt; migration continues | Copied corrupt-store fixture |

## PR and task sequence

### PR 18B: Crate contracts, grammar/extractor registry, and deterministic extraction

**Files:** create `Cargo.toml`, `src/{lib,error,grammar,registry,identity}.rs`, `src/extract/{mod,driver,queries}.rs`, initial `src/extract/langs/` set (Rust, TypeScript/JavaScript, Python first), `tests/extraction_conformance.rs`; modify workspace `Cargo.toml`.

- [ ] Write failing tests named `same_content_same_versions_extracts_identical_rows`, `registry_rejects_duplicate_language_owner`, `registry_rejects_abi_mismatch`, `occurrence_id_is_deterministic`, `symbol_seed_disambiguates_overloads`, `redacted_file_keeps_structural_identity`, `redacted_file_never_retries_raw_content`, `parse_budget_degrades_to_partial_coverage`, and `unknown_language_is_unsupported_coverage`.
- [ ] Add the public signatures above with serde tags fixed to `snake_case` and no `Serialize` on any transient parse structure.
- [ ] Implement grammar descriptors pinned to the bundled 0.26-line grammar crates with tier features mirroring V1's `default`/`medium`/`full` sets; record the registry digest.
- [ ] Port language semantics from the exact V1 `src/extraction` seams without importing V1 modules; add per-language redacted golden fixtures asserting canonical extraction rows.
- [ ] Add architecture lint rejecting imports of `tracedecay::db`, `tracedecay::extraction`, `tracedecay::graph`, `rusqlite`, `tracedecay-store`, `tracedecay-capture`, and any network crate.
- [ ] Run `cargo test -p tracedecay-code-index --test extraction_conformance`; expected: exit 0 and fixture manifest hashes match on two consecutive runs.
- [ ] Run `cargo clippy -p tracedecay-code-index --all-targets --all-features -- -D warnings`; expected: exit 0 with no warnings.
- [ ] Commit `feat(code-index): add deterministic extraction registry`.

### PR 18C: Watcher intake, incremental reuse, and bounded overlays

**Files:** create `src/{intake,incremental,overlay}.rs`, `tests/incremental_suite.rs`; extend `benches/code_index.rs`.

- [ ] Write failing tests named `unchanged_reuse_key_skips_reparse`, `grammar_bump_invalidates_only_its_languages`, `resolver_bump_reresolves_without_reparse`, `full_rebuild_requires_declared_reason`, `overlay_depth_beyond_bound_is_compaction_eligible`, `intake_coalesces_per_lane`, `intake_rejects_storm_with_visible_count`, `explicit_scope_never_falls_back_to_cwd`, and `dirty_set_is_exact_not_directory_wide`.
- [ ] Implement reuse keys, the invalidation matrix, `FullRebuildReason`, overlay planning against plan 02's ADR depth constants, and the intake planner with per-repository lanes.
- [ ] Wire the root daemon glue signature (`src/v2_adapters/code_index.rs`) so the planner's `CaptureSnapshot` decisions schedule plan 03 `code_snapshot` capture runs; the crate itself gains no I/O.
- [ ] Run `cargo test -p tracedecay-code-index --test incremental_suite`; expected: exit 0; storm fixture holds peak memory below the declared bound.
- [ ] Run `cargo bench -p tracedecay-code-index --bench code_index -- incremental`; expected: report records single-file-change re-index p95 at current scale and the reused/reparsed file counts.
- [ ] Commit `feat(code-index): add intake and incremental planning`.

### PR 18D: Generation build plans, canonical rows, digests, and packed schema

**Ordering:** consumes plan 02 PR 6C (`GraphGenerationRepository`) through the port; lands before plan 04 PR 18 wires `code_evidence_v1` to the real builder.

**Files:** create `src/build/{mod,rows,digest}.rs`, `src/resolve.rs`, `tests/generation_suite.rs`; extend `benches/code_index.rs`.

- [ ] Write failing tests named `two_builds_same_inputs_same_digest`, `digest_ignores_compression_and_layout`, `row_order_is_total_and_fixed`, `seal_rejects_manifest_digest_mismatch`, `edge_targets_are_exactly_one_of_occurrence_entity_unresolved`, `every_content_row_binds_a_receipt`, `overlay_build_never_mutates_base_generation`, and `parallel_build_matches_serial_digest`.
- [ ] Implement `GenerationBuildPlanV1`, streaming canonical row emission for every table in the packed schema above, edge resolution with retained unresolved targets, and the generation digest.
- [ ] Land the column-level generation schema (this PR is the owning implementation PR, per plan 02's schema-ownership rule, for `generation_manifest`, `gen_snapshots`, `gen_file_content`, `gen_files`, `gen_symbol_occurrences`, `gen_edges`, `gen_diagnostics`, `gen_tests`/`gen_test_map`, `overlay_journal`/`overlay_files`).
- [ ] Run `cargo test -p tracedecay-code-index --test generation_suite`; expected: exit 0 and identical digests across two full builds and across serial-vs-parallel builds.
- [ ] Run `cargo bench -p tracedecay-code-index --bench code_index -- build`; expected: current-scale full build and 10× symbol-scale build record throughput, peak RSS, and digest stability.
- [ ] Commit `feat(code-index): add deterministic generation builds`.

### PR 18E: Symbol lineage, diagnostics mapping, and test attribution

**Files:** create `src/{lineage,diagnostics,test_attribution}.rs`, `tests/lineage_attribution_suite.rs`.

- [ ] Write failing tests named `rename_produces_candidate_not_merge`, `move_across_files_keeps_entity_via_lineage`, `split_and_merge_stay_candidate_sets`, `ambiguous_lineage_never_auto_merges`, `exact_fingerprint_move_is_derived_exact`, `diagnostic_maps_only_within_diagnosed_snapshot`, `unindexed_snapshot_diagnostic_is_coverage`, `recorded_run_and_static_inference_stay_distinct_classes`, and `attribution_ratios_expose_evidence_class`.
- [ ] Implement lineage candidate computation with the five evidence kinds, diagnostic attribution with containment mapping, and dual-class test attribution.
- [ ] Freeze the labeled lineage corpus shared with plan 04's F1 ≥ 98% release gate and record its manifest hash.
- [ ] Run `cargo test -p tracedecay-code-index --test lineage_attribution_suite`; expected: exit 0 and corpus F1 report emitted with per-kind breakdown.
- [ ] Commit `feat(code-index): add lineage and attribution mapping`.

### PR 18F: V1 differential parity, scale benchmarks, and convergence evidence

**Files:** create `tests/migration_parity.rs` (fixture half), extend `tests/{extraction_conformance,generation_suite}.rs`; add copied V1 golden manifests.

- [ ] Build differential fixtures from copied V1 branch graph stores: node/edge/file counts, qualified names, edge kinds, diagnostics, test-map answers, and `test_risk`-class outputs, classified `exact`, `expected_normalization`, `v1_bug_preserved`, or `unexplained`; `unexplained` fails.
- [ ] Assert V1 suites stay green during shadow: run `cargo test --test extraction_suite --test graph_suite`; expected: exit 0 because shadow indexing changes no V1 writes.
- [ ] Benchmark 2/8/32-repository federated openings against the plan 02 open-generation limit and record per-repository open counts.
- [ ] Run `cargo test -p tracedecay-code-index --test migration_parity fixtures`; expected: every V1 fixture row has a disposition and zero `unexplained`.
- [ ] Commit `feat(code-index): prove v1 extraction parity`.

### PR 33G: V1 branch graph store migration, dispositions, and disk math

**Ordering:** runs inside plan 12's PR 33R controller phases; consumes plan 12 PR 3R inventory; precedes plan 12 §15 PR 37C retirement.

**Files:** create `src/migrate_v1.rs`; extend `tests/migration_parity.rs`; migration receipts land in the execution PR's generated manifests.

- [ ] Write failing tests named `resolvable_commit_prefers_reindex_over_import`, `unresolvable_branch_imports_with_v1_provenance`, `durable_fact_rows_block_store_archive`, `every_store_gets_exactly_one_disposition`, `identity_split_quarantines_not_duplicates`, `corrupt_store_is_quarantined_disposition`, and `disk_amplification_within_gate`.
- [ ] Implement the re-index-first policy, import readers keyed by V1 schema version lineage, and per-store disposition emission in plan 12's manifest vocabulary (`retained | skipped | quarantined | redacted | deleted`).
- [ ] Bind receipts to the plan 14 §2 `FM-###` rows for #269/#371 and #406; record V1 DB hashes and PR #405 adoption identities in the import manifest.
- [ ] Produce the disk-math receipt: total V1 branch-store bytes vs packed generation + overlay bytes, proving ≤ 2.25× migration amplification and the ≤ 1.2× steady-state target at current scale.
- [ ] Run `cargo test -p tracedecay-code-index --test migration_parity`; expected: exit 0 with a machine-readable disposition manifest covering 100% of inventoried stores.
- [ ] Commit `feat(code-index): migrate v1 branch graph stores`.

## Compatibility, cutover, and rollback rules

- V1 extraction and per-branch stores remain authoritative for V1 surfaces until plan 12's code/graph bounded-context cutover (PR 35 series) accepts the parity receipt; shadow indexing never mutates V1 stores.
- Cutover switches plan 05 code queries to published V2 generations per repository family; stale clients and retired V1 tool names fail with the typed current-capability errors owned by plan 17's stale-client error registry, never with a live V1 fallback path.
- Rollback re-points reads at the V1 stores from the migration receipt without deleting V2 generations; resumed shadow indexing starts a new build epoch, and prior generations remain within plan 02's rollback window.
- V1 branch graph DBs stay read-only for one release after verified cutover; deletion is PR 37C with archive-restore proof, and durable graph-resident fact rows must be verifiably migrated first (plan 12 §15).

## Release gates

### Determinism and correctness

- Two full builds at the same inputs produce identical `GenerationDigest`, canonical row streams, counts, and coverage on two different machines; parallel and serial builds match.
- Second extraction of every fixture yields byte-identical rows; second migration run of every copied store yields zero new imports (idempotent by V1 DB hash + disposition).
- Every generation row that names an occurrence/edge resolves referentially within its snapshot; unresolved edge targets are retained, counted, and queryable, never dropped.
- Lineage: labeled-corpus F1 at or above 98% (shared gate with plan 04); ambiguous candidates are 100% visible; zero silent merges in the fixture set.
- V1 differential parity: zero `unexplained` rows across extraction, graph, diagnostics, and test-map fixtures.

### Performance and scale

- Current-scale (978 files / 36k nodes / 71k edges) full extraction+build completes within the benchmark budget recorded with the reference machine; single-file incremental re-index p95 is recorded and bounded.
- 10× gate: 1M-symbol project builds within recorded budget; peak query-side RSS stays within master §26's 1.5 GB envelope (measured with plan 05's harness).
- ≤ 10,000 generation/overlay files per profile after compaction; ≤ 8 generation files open per query process; overlay depth never exceeds the ADR bound.
- Migration disk amplification ≤ 2.25×; steady-state packed size ≤ 1.2× the largest single V1 branch store at current scale.

### Privacy

- The crate consumes only receipt-bound sanitized content; the committed secret corpus yields zero secret-bearing occurrence names, signatures, snippets, FTS rows, or fingerprint inputs across every generation table (plan 18 §11.2 obligations).
- Redacted files retain structural identity only when zero candidate bytes remain; ignore policies reduce scope but never make included secret content indexable.
- Generation tables store privacy-domain-keyed fingerprints only; no unkeyed content hash exists in any row, log, or manifest.

### Observability

- Metrics expose intake lane depth/debounce/storms, files parsed/reused/degraded per language, extraction coverage by `ExtractionCoverage` kind, build duration/rows/digest, overlay depth/compaction eligibility, index freshness watermark per repository, lineage candidate counts, and migration dispositions.
- Every index answer surface can report `CoverageReportV1` (plan 01): searched/skipped/unavailable/stale/truncated/redacted with freshness watermarks — an unindexed snapshot is explicit coverage, never an empty success.
- Logs carry language IDs, versions, counts, and keyed fingerprints only; never file paths joined with content, symbol bodies, or diagnostic message literals.

## Definition of done

- `tracedecay-code-index` exists with the exact module layout, passes registry validation for every built-in language tier, and owns extraction/incremental/overlay/build/lineage/attribution semantics with no duplicate implementation left in `src/extraction/**`, `src/db/**` graph paths, or `src/diagnostics/**` after retirement.
- The sanctioned pipeline is the only repository-content path: plan 03 `code_snapshot` adapter → sanitizer → this indexer → plan 02 generations → plan 05 queries; architecture lints prove no bypass.
- Generation builds are deterministic, streaming, receipt-bound, and executed only under plan 04's `code_evidence_v1` transaction through `GenerationBuilderPort`.
- Packed generations + bounded overlays + ref pointers replace per-branch databases; the scale envelope and file-count/open-handle/disk gates hold at current and 10× scale.
- Symbol identity survives moves/renames through evidence-bearing lineage candidates; identity collisions quarantine per plan 03's vocabulary; the #269/#371 and #406 regression classes have bound `FM-###` receipts.
- Diagnostics and tests map to exact snapshot occurrences with dual evidence classes preserved end-to-end into plan 05 answers.
- PR 33G migrated, disposed, and disk-proved every inventoried V1 branch graph store; PR 37C retirement preconditions (durable-row migration, archive restore) are satisfied; plan 19 §12.3 deletion-wave evidence lists the V1 graph stack.
- All release gates above pass on copied real stores and the redacted fixture corpus, and V1 `extraction_suite`/`graph_suite` remained green throughout the shadow window.
