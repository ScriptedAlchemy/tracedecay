# TraceDecay V2 Capture Crate Implementation Plan

**Goal:** Build `tracedecay-capture`, the deterministic, privacy-first boundary that discovers V1 and live provider artifacts, durably spools high-volume hook events, and commits idempotent `ObservationEnvelopeV1` records without owning canonical events or read projections.

**Architecture:** Provider adapters discover, frame, and parse source records into transient `Unclassified` drafts. The single Plan 18 sanitizer classifies structured fields, redacts or drops content, issues `SanitizationReceiptV1`, and creates a `Sanitized` observation before any general spool/blob/journal write. A shared normalizer assigns source identity, rewrite generation, offsets, hashes, privacy/retention, and replay metadata before an `ObservationSink` transaction publishes observations and outbox rows. Hook processes use the same mandatory sanitizer before a bounded append-only spool; asynchronous drainers reuse the same journal path as transcript, Git, LCM, and automation importers.

**Tech Stack:** Rust workspace; `tracedecay-domain` contracts; `serde`/`serde_json`; SHA-256 and UUID namespaced identity through domain helpers; `rusqlite`-backed sink supplied by `tracedecay-store`; private append-only spool segments; property tests, redacted golden fixtures, crash tests, and Criterion benchmarks.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) requires capture of provider-native goals/plans/workflows, executor registration/lifecycle observations, workspace/Git/delivery facts, tool effects, costs, and external task-system records as sanitized evidence. Capture never materializes schedulable work, assigns an executor, grants authority, or treats a provider/board status as canonical completion.

---

## Goals

- Capture every supported V1 provider/source family without changing V1 writes during shadow mode.
- Open and scan each provider-native source once per committed source frontier, independent of how many projects, worktrees, queries, or concurrent refresh requesters may consume its attributed evidence.
- Use one domain `ScopeSelectorV2` for multi-repo/project/checkout/worktree/ref/snapshot/generation discovery; source candidates never collapse to current project, `project_key`, first CWD, active base checkout, or current graph.
- Make an observation deterministic from source instance, artifact identity, rewrite generation, record offset/sequence, and privacy-domain-keyed source fingerprint; any raw checksum is transient/non-serializable inside sanitizer memory.
- Acknowledge a source offset only in the same commit that persists the observation and its outbox row.
- Preserve late, duplicate, rewritten, malformed, partial, unknown-version, and out-of-order evidence without silent loss or fabricated order.
- Keep hook synchronous capture p95 at or below 8 ms — plan 07's capture sub-budget inside its 10 ms notification-hook total — while many parent/subagents emit concurrently; the 10 ms spool deadline remains the hard synchronous cutoff.
- Parse, classify, and sanitize through one versioned engine before any general persistence, FTS, vectors, facts, fixtures, exports, logs, policy/hint input, or projector input can see content.
- Represent only provider/host-exposed reasoning artifacts; never infer, decrypt, or reconstruct hidden chain-of-thought.
- Produce replay manifests using domain `ReplayMode::{ExactDeterministic, RecordedResult, CurrentBestEffort}` unchanged; exact replay, recorded-result inspection, and current best-effort rerun cannot silently degrade into one another.
- Shadow V1, prove provider and aggregate parity, cut over source-offset ownership independently, and roll back from a migration receipt.

## Non-goals

- No canonical entity resolution, canonical event projection, relation inference, search indexing, ranking, or UI read models.
- No direct dependency on CLI, MCP, dashboard, HTTP, policy, or V1 storage types.
- No unsanitized or third-party transcript upload, direct cloud-database write, required remote daemon, or network call on the hook hot path. Plan 28 permits policy-eligible sanitized observation batches to drain asynchronously to an enrolled TraceDecay authority.
- No parsing of encrypted reasoning payloads and no labeling ordinary assistant text as reasoning.
- No deletion of V1 sources, V1 stores, hook JSONL, or automation files during capture cutover.
- No cross-shard transaction; the sink commits one owning shard and reports its outbox sequence.

## Convergence boundary

Capture is the sole runtime content-ingress/sanitizer owner in [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md) and [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md). It consumes the exact domain taint/scope/evidence types from [`01-domain-crate.md`](01-domain-crate.md), uses store ports from [`02-store-crate.md`](02-store-crate.md), and emits only observations for [`04-projectors-crate.md`](04-projectors-crate.md). Scout/model/delivery evidence from [`22`](22-incremental-context-scout-and-suggestion-envelopes.md), occurrence/correction/summary evidence required by [`23`](23-session-lcm-temporal-retrieval-and-evaluation.md), and multi-machine sync envelopes/receipts from [`28`](28-remote-multi-machine-shared-brain.md) enter through the same sanitized observation contract; capture never ranks, addresses, authorizes, or projects them.

Capture consumes the capture facet of plan 08/27's one canonical `HostIntegrationManifestV1`; hooks, installers, skills/roles, MCP facades, and executors consume sibling facets from that same host/version/event/capability identity, while generated `HostBundleManifestV1` artifacts only reference it. Capture owns parser/source descriptors and offsets, not a competing host registry. Shared adapter mechanics—artifact discovery limits, bounded decoding/framing, offset/rewrite continuity, structured-field maps, normalization, sanitizer call, coverage, and conformance runner—are implemented once in `source.rs`/`runner.rs`; provider modules contain only true wire/schema differences.

| Boundary | Contract |
|---|---|
| Enters | Provider-owned source artifacts, bounded raw records in transient memory, explicit `ScopeSelectorV2`, source state, privacy policy/detector snapshot, and store ports. |
| Exits | Sanitized immutable observations with receipts, source continuity/cursors, non-content quarantine skeletons, optional opaque protected refs, outbox entries, coverage, and replay manifests. |
| Upstream owner | Domain owns types; Plan 18 owns security invariants; providers own raw source truth; application supplies authorized scope/policy snapshots. |
| Downstream owner | Projectors alone create canonical entities/events/relations; query/policy/API never invoke provider parsers or alternate redactors. |
| Extension seam | A provider adds a descriptor, structured field map, bounded parser, sanitizer conformance cases, source identity/rewrite rules, capability/coverage declaration, and redacted fixtures; it cannot add its own detector or journal schema. |
| Scale/concurrency | Independent per-source/producer lanes, bounded parsing/scanning/spooling, fair drains, idempotent journal commits, gap/rewrite evidence, and no cross-agent/global ordering. |
| Migration/retirement | V1 adapters are read-only sources and differential fixtures. Cut over source cursor ownership per family, then retire duplicate V1 parser/redactor/live paths after parity/privacy receipts; provider raw source remains provider-owned. |

## Cross-crate contract

### Consumes

- `tracedecay-domain`: `Unclassified`/`Classified`/`Sanitized`, `SanitizationReceiptV1`, sink-eligible types, `ObservationEnvelopeV1`, source/provider identifiers, timestamps, privacy/retention classes, payload discriminators/references, and replay-mode vocabulary.
- `tracedecay-store` through capture-owned ports: atomic observation/outbox append, sanitized blob staging/publication, isolated protected-quarantine operations, non-content quarantine records, source-state compare-and-set, and spool acknowledgement storage.
- V1/read-only sources: provider transcripts, global sessions, LCM rows and payloads, Git state, hook JSONL, analytics, automation ledgers/artifacts, and compatibility inventory manifests.

### Produces

- Immutable sanitized observations plus one transactional outbox entry per newly committed observation; every content-bearing envelope binds one complete sanitization receipt.
- Durable source cursors, rewrite-generation receipts, duplicate/gap/late markers, quarantine entries, and coverage metrics.
- `CaptureReplayManifestV1` records consumed by Ingest Lab, parity tooling, and deterministic projector rebuild tests.
- No canonical event, entity, relation, search document, vector, or aggregate row.

The dependency boundary is `tracedecay-domain <- tracedecay-capture`; store implementations are injected by the root/application composition crate. `tracedecay-capture` may not import `src/sessions`, `src/hooks`, `src/automation`, `src/mcp`, or `src/dashboard`.

In `DedicatedServiceIdentity` deployments, filesystem/provider discovery and `SourceAdapter` execution run in plan 12 PR 24E0's user-side source-broker composition, not in the daemon identity. The broker is granted only registered source/repository capabilities, runs this crate's normalize/sanitize/framing path, and submits typed receipt-bearing observations or code snapshots over authenticated local capture ports. Provider-source SQLite readers remain read-only adapters here; neither they nor the broker can import TraceDecay store layout, `StoreFactory`, canonical repositories, or database paths. The daemon therefore needs no broad user-home access, while capture semantics and sanitization remain identical to portable mode.

## Exact crate and module layout

| File | Responsibility |
|---|---|
| `crates/tracedecay-capture/Cargo.toml` | Crate dependencies and features; no default network feature. |
| `crates/tracedecay-capture/src/lib.rs` | Public exports only. |
| `crates/tracedecay-capture/src/error.rs` | Typed discovery, framing, parsing, privacy, spool, journal, and compatibility errors. |
| `crates/tracedecay-capture/src/source.rs` | `SourceAdapter`, artifact, record, cursor, scan-budget, and batch contracts. |
| `crates/tracedecay-capture/src/identity.rs` | Deterministic source-instance, artifact, rewrite-generation, idempotency, and observation-ID derivation. |
| `crates/tracedecay-capture/src/normalize.rs` | Shared source/identity/record-to-draft pipeline; returns transient `Unclassified` content only. |
| `crates/tracedecay-capture/src/privacy/**` | The sole Plan 18 structured parser/classifier/detector/redactor, policy, receipt, bounded plugin, sink eligibility, and protected-routing implementation. |
| `crates/tracedecay-capture/src/journal.rs` | Capture-owned `ObservationSink` and atomic append/source-state contract. |
| `crates/tracedecay-capture/src/runner.rs` | Discovery, bounded scanning, normalization, commit, retry, and source acknowledgement. |
| `crates/tracedecay-capture/src/spool/{mod,client,frame,recovery}.rs` | Framed private spool segments, hook-facing append client, per-producer sequence allocation, ack compaction, overflow lanes, and recovery. |
| `crates/tracedecay-capture/src/hook.rs` | Synchronous hook append API and asynchronous spool drainer. |
| `crates/tracedecay-capture/src/framed_log.rs` | One invariant-heavy framed-segment codec/recovery kernel: sequence/prior-digest, CRC/HMAC/AEAD hooks, append+fsync, torn-tail scan, bounded rotation, and segment retirement. The hook spool supplies its frame registry/storage policy; root lifecycle bootstrap reuses the kernel with a closed receipt registry and separate service-only root/key. |
| `crates/tracedecay-capture/src/quarantine.rs` | Stable non-content quarantine reason/coverage skeletons and retry eligibility; optional bytes live only behind the store's isolated `ProtectedSecretRef`. |
| `crates/tracedecay-capture/src/replay.rs` | Exact/recorded/best-effort capture replay manifests and substitution reporting. |
| `crates/tracedecay-capture/src/shadow.rs` | V1/V2 dual-read comparison, freeze watermarks, migration receipts, cutover, and rollback. |
| `crates/tracedecay-capture/src/adapters/mod.rs` | Complete adapter registry and provider/source capability matrix. |
| `crates/tracedecay-capture/src/adapters/codex.rs` | Codex JSONL/app-server events, response items, goal events, turn context, tool/reasoning records. |
| `crates/tracedecay-capture/src/adapters/claude.rs` | Claude transcripts, visible thinking blocks, hook markers, PR links, compact/model-fallback markers, subagents. |
| `crates/tracedecay-capture/src/adapters/cursor.rs` | Cursor agent JSONL, project attribution candidates, dispatch/subagent events, model/timestamp carry. |
| `crates/tracedecay-capture/src/adapters/cursor_composer.rs` | Cursor Composer SQLite/envelope/store-vscdb read-only framing and plans/tools/Git metadata. |
| `crates/tracedecay-capture/src/adapters/cline_like.rs` | Cline-family transcript framing. |
| `crates/tracedecay-capture/src/adapters/hermes.rs` | Hermes transcript source under `~/.hermes`; runtime ownership resolves to the ordinary user-profile activity/project shards. |
| `crates/tracedecay-capture/src/adapters/kiro.rs` | Kiro transcript and hook records. |
| `crates/tracedecay-capture/src/adapters/vibe.rs` | Vibe transcript records. |
| `crates/tracedecay-capture/src/adapters/hook_events.rs` | Codex/Claude/Cursor/Kiro hook event framing and producer/session/agent identity hints. |
| `crates/tracedecay-capture/src/adapters/lcm_v1.rs` | V1 raw-message, summary DAG, source range, compression, lifecycle, payload, and tombstone observations. |
| `crates/tracedecay-capture/src/adapters/git.rs` | Repository/worktree/ref/commit and fetched delivery evidence snapshots. |
| `crates/tracedecay-capture/src/adapters/code_snapshot.rs` | Code-snapshot extractor: frames tracked-file text and bounded dirty overlays at explicit repository/checkout/worktree/ref/snapshot tuples so repository content crosses the capture sanitizer before the [`25-code-intelligence-indexing-crate.md`](25-code-intelligence-indexing-crate.md) indexer consumes it. |
| `crates/tracedecay-capture/src/adapters/automation.rs` | Config, scheduler, run ledger, artifacts, proposals, approvals, skills, facts, and outcome files. |
| `crates/tracedecay-capture/src/adapters/v1_sessions.rs` | V1 global session/message/parse-offset/analytics backfill rows. |
| `crates/tracedecay-capture/tests/contract_suite.rs` | Source identity, rewrite, offset, commit, replay, quarantine, and adapter-registry contracts. |
| `crates/tracedecay-capture/tests/hook_spool_suite.rs` | Contention, crash, ack, overflow, gap, duplicate, late, and recovery tests. |
| `crates/tracedecay-capture/tests/provider_conformance.rs` | Redacted golden fixture matrix for every registered adapter. |
| `crates/tracedecay-capture/tests/shadow_parity.rs` | Copied V1-store manifests and per-provider/aggregate parity. |
| `crates/tracedecay-capture/benches/capture.rs` | Hook latency, transcript throughput, redaction, spool drain, and concurrent-agent benchmarks. |

Root-composition companion glue is `src/v2_adapters/capture_store.rs`: it implements capture-owned `ObservationSink` over store `ObservationJournal`/blob/quarantine ports. Neither capture nor application imports a concrete store implementation, and the adapter adds no parsing, identity, retry, or policy semantics.

`framed_log` is a mechanical kernel, not a second semantic journal owner. It knows no hook, lifecycle, checkpoint, store, or receipt type; callers provide a closed frame registry and policy. The capture spool and root lifecycle bootstrap therefore share byte framing, fsync, torn-tail, and rotation tests without sharing paths, keys, retention, authorization, acknowledgements, or domain state.

Phase 0's reuse ledger inventories provider switch statements, duplicate file readers/decoders, host-name/capability lists, offset stores, and redactors. Every adapter cutover records handwritten V1/adaptor/V2 lines and deleted call sites; a shared runner that still delegates to duplicate live V1 parsing does not pass. The provider conformance matrix is generated from the registry so adding a provider cannot require another hand-written test/router/permission list.

## Public API and fixed signatures

```rust
pub trait SourceAdapter: Send + Sync {
    fn descriptor(&self) -> &'static SourceDescriptor;
    fn discover(
        &self,
        scope: &ScopeSelectorV2,
        cursor: &DiscoveryCursor,
    ) -> Result<Vec<SourceArtifact>, CaptureError>;
    fn scan(
        &self,
        artifact: &SourceArtifact,
        cursor: &SourceCursor,
        budget: ScanBudget,
    ) -> Result<SourceBatch, CaptureError>;
    fn normalize(
        &self,
        artifact: &SourceArtifact,
        record: SourceRecord,
        context: &NormalizeContext,
    ) -> Result<Unclassified<ObservationDraft>, CaptureError>;
}

pub struct SourceDescriptor {
    pub adapter_id: &'static str,
    pub adapter_version: &'static str,
    pub source_system: SourceSystem,
    pub provider: Option<ProviderId>,
    pub record_families: &'static [RecordFamily],
    pub ordering: SourceOrdering,
    pub required_host_capabilities: &'static [CapabilityId],
}

pub struct SourceAdapterExecutionRefV1 {
    pub descriptor_digest: ManifestDigest,
    pub adapter_id: RegistryEntryId,
    pub adapter_version: ComponentVersion,
    pub host_integration: Option<HostIntegrationRuntimeRefV1>,
    pub host_capability_snapshot_digest: Option<ManifestDigest>,
    pub execution_ref_digest: ManifestDigest,
}

pub struct NormalizeContext {
    pub execution: SourceAdapterExecutionRefV1,
    pub host_capabilities: Option<HostCapabilitySnapshotV1>,
    pub replay_manifest_digest: ManifestDigest,
}

pub struct SourceArtifact {
    pub source_instance: SourceInstanceId,
    pub artifact_id: ArtifactId,
    pub privacy_domain: PrivacyDomainId,
    pub locator: SourceLocator,
    pub identity_fingerprint: PrivacyDomainBoundLocatorDigest,
    pub head_fingerprint: KeyedSourceRecordFingerprint,
    pub observed_len: u64,
    pub observed_modified_at: Option<UtcMicros>,
}

pub struct SourceRecord {
    pub position: SourcePosition,
    pub occurred_at: OccurredAt,
    pub encoding: RecordEncoding,
    pub bytes: Vec<u8>,
}

pub struct SourceBatch {
    pub generation: RewriteGeneration,
    pub records: Vec<SourceRecord>,
    pub next_cursor: SourceCursor,
    pub completeness: BatchCompleteness,
    pub detected_gaps: Vec<SequenceGap>,
}

pub struct ScanBudget {
    pub max_artifacts: NonZeroU32,
    pub max_records: NonZeroU64,
    pub max_input_bytes: NonZeroU64,
    pub max_wall_time: Duration,
    pub yield_every_records: NonZeroU32,
    pub cancellation: Arc<dyn CaptureCancellation>,
}

pub trait CaptureCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

pub struct CaptureRequest {
    pub scope: ScopeSelectorV2,
    pub discovery_cursor: DiscoveryCursor,
    pub scan_budget: ScanBudget,
    pub replay_mode: ReplayMode,
}

pub trait ObservationSanitizer: Send + Sync {
    fn sanitize(
        &self,
        draft: Unclassified<ObservationDraft>,
        context: &SanitizationContext,
    ) -> Result<SanitizedObservation, CaptureError>;
}

pub struct SanitizedObservation {
    pub envelope: ObservationEnvelopeV1,
    pub receipt: SanitizationReceiptV1,
    // Move-only sanitizer output; capture stages it through the narrow sink
    // before constructing the append item's private attachment token.
    pub protected: Option<ProtectedQuarantineIngress>,
}
```

`ScanBudget` is mandatory for live and backfill scans. Hitting a record/byte/artifact/time limit or cooperative cancellation returns `BatchCompleteness::Partial` plus the last fully framed resumable position; it never advances `AppendReceipt.post_commit_source_head` for an uncommitted record. The runner schedules by `SourceInstanceId` and committed frontier, not destination project. Equivalent refresh demand joins plan 09's daemon-owned fenced operation; capture does not implement handler-local singleflight. One sanitized canonical activity observation may later acquire zero-to-many project/worktree attributions through plan 04, so scan-once never means copying a transcript body into many project stores.

```rust
pub trait ObservationSink: Send + Sync {
    fn source_state(&self, key: &SourceKey) -> Result<Option<SourceHeadV1>, CaptureError>;
    fn stage_protected(
        &self,
        request: ProtectedQuarantineWrite,
        content: ProtectedQuarantineIngress,
    ) -> Result<ProtectedQuarantineAttachmentV1, CaptureError>;
    fn commit(&self, batch: ObservationAppendBatchV1) -> Result<CaptureCommitReceipt, CaptureError>;
}

pub struct CaptureCommitReceipt {
    pub append: tracedecay_domain::AppendReceipt,
}

pub struct CaptureRunner<A, S> {
    adapter: A,
    sanitizer: Box<dyn ObservationSanitizer>,
    sink: S,
    policy: CapturePolicy,
}

impl<A: SourceAdapter, S: ObservationSink> CaptureRunner<A, S> {
    pub fn capture(&self, request: CaptureRequest) -> Result<CaptureReport, CaptureError>;
}
```

`CaptureRunner` passes `CaptureRequest.scope` unchanged to discovery and records its canonical digest in the capture manifest. Adapters may emit zero-to-many attributed scope candidates with source-field/record evidence, but cannot replace or narrow the requested repository/project/checkout/worktree/ref/snapshot/generation set. An empty selector is rejected before discovery; ambiguity, stale registry candidates, and missing selected artifacts are report coverage, not a current-CWD fallback.

`SourcePosition` is imported from plan 01; adapters are its only constructors and do not redeclare it. `ObservationSink::commit` receives the same plan-01 `ObservationAppendBatchV1` consumed by plan 02. Every item carries its envelope, exact `ProvenanceV1`, exact `SanitizationReceiptV1`, and optional non-content quarantine disposition; the batch carries expected/next `SourceHeadV1` state plus the complete schema-bound replay manifest and its digest. No capture-to-store conversion may drop those fields. The store rehashes the manifest, validates provenance against envelope/source/fingerprint/parser/time, compare-and-sets the full expected source head, inserts provenance plus receipt plus envelope plus quarantine skeleton, derives one registry-authorized outbox intent per new observation, and advances the head in one transaction. `AppendReceipt.post_commit_source_head` is the sole acknowledged cursor authority: `Gap`/`Late` can commit evidence without advancing it. A crash before commit leaves the previous head; a crash after commit returns the existing receipt on retry.

`ObservationSanitizer` is the only implementation permitted to construct `SanitizedObservation` or mint `SanitizationReceiptV1`. For a protected candidate, `CaptureRunner` moves its non-cloneable `ProtectedQuarantineIngress` through `ObservationSink::stage_protected`, then moves the returned private attachment token into `ObservationQuarantineDispositionV1`; ordinary observations require no staging. It moves every envelope/receipt/disposition intact into `ObservationAppendItemV1`. Capture mints every receipt; the durable receipt home is the per-shard `sanitization_receipts` table defined in [`02-store-crate.md`](02-store-crate.md), and `ObservationSink::commit` persists each receipt in the same transaction as its envelope. Plan [`04-projectors-crate.md`](04-projectors-crate.md)'s sink firewall validates receipts against that table; [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md) defines the receipt's fields and invariants. Adapters parse and identify structured fields but cannot classify eligibility themselves. `ObservationSink` rejects an envelope whose receipt, output digest, privacy domain, parser/detector/policy digest, completeness, or protected attachment token does not match. Incomplete/timeout/unsupported scans become receipt-bearing non-content items inside the same append batch; only unattached encrypted staging can remain after failure, and the protected service retires it without advancing the source head.

Every normalized observation and replay-manifest member carries the exact `SourceAdapterExecutionRefV1` that produced it. For an installed host-derived source this pins the shared plan-01 `HostIntegrationRuntimeRefV1`, component-set and bundle payload/signed-release digests, adapter version, and install receipt/generation; `NormalizeContext.host_capabilities.subject` must be the matching `Installed` runtime and its independently pinned snapshot digest must be fresh at scan start. A provider-owned or pre-install source records no invented runtime: it uses the `Target` capability subject plus descriptor/adapter digests where a probe exists, or `None` with explicit coverage otherwise. These fields are provenance and replay inputs, not identity inputs: updating or reinstalling an adapter cannot mint a second observation for the same provider-native record, while a replay can still distinguish the exact parser/bundle/probe that produced an old result. Projectors expose stale/unsupported adapter provenance as coverage rather than silently treating it as current.

### Deterministic identity, rewrite, offsets, and ordering

```rust
pub struct CaptureObservationIdentity {
    pub source_instance: SourceInstanceId,
    pub artifact_id: ArtifactId,
    pub generation: RewriteGeneration,
    pub position: SourcePosition,
}

pub fn lower_observation_key(input: &CaptureObservationIdentity) -> tracedecay_domain::ObservationKey;
pub fn detect_rewrite(
    committed: Option<&SourceHeadV1>,
    artifact: &SourceArtifact,
) -> RewriteDecision;
```

`SourceRecord.bytes` and any transient checksum exist only in bounded capture/sanitizer memory and cannot implement `Serialize`, `Display`, logging, repository, or receipt traits. The sanitizer computes `KeyedSourceRecordFingerprint` with the privacy-domain key after parsing/classification. The fingerprint enters envelope/provenance/source-head verification fields, never `CaptureObservationIdentity` or `ObservationKey`; observation identity remains stable across key rotation.

- `SourceInstanceId` is a namespaced deterministic ID over TraceDecay `ProfileId`, optional `HostProfileRef` source partition, host installation, adapter ID, and provider-native source instance. Zero/one/many Hermes host profiles remain source partitions inside one TraceDecay profile; none is a data owner or a TraceDecay profile.
- `ArtifactId` is a namespaced deterministic ID over source instance plus the provider-native durable artifact identity; a pathname is only one alias.
- The adapter normalizes the four `CaptureObservationIdentity` fields into the domain `ObservationKey` canonical field encoding; `derive_observation_id` is the only observation-ID implementation. Capture may not define a second UUID namespace or canonical encoder.
- `SourcePosition` persists through the offset-lowering columns defined in [`02-store-crate.md`](02-store-crate.md): `observations` stores `(position_kind TEXT, byte_start INTEGER NULL, byte_end INTEGER NULL, object_key_digest BLOB NULL)` with `source_heads.contiguous_offset` retained only for byte/row/sequence ordered sources. Capture treats the lowering as opaque and round-trips every variant, including keyed `ObjectKey(PrivacyDomainBoundLocatorDigest)` and `ByteOffset{start,end}`.
- Key rotation never changes an `ObservationId`. Under the lifecycle lease, the sanitizer/key service can recompute active-source fingerprints with old and new epochs while authorized source bytes are transiently available, append a signed `FingerprintEpochContinuityV1` receipt, and advance only the verification fingerprint/head epoch. If continuity cannot be proven, capture starts a named rewrite generation or quarantines; it never compares raw digests, silently treats an epoch mismatch as a rewrite, or duplicates the prior generation.
- Append growth with matching artifact and head fingerprints preserves the generation and resumes at the committed cursor.
- Every ordered provider cursor is a total, replay-stable composite over the provider's strongest monotonic position plus a deterministic tie-breaker (for example native timestamp plus native row ID/sequence). Timestamp-only, mtime-only, count-only, or `LIMIT/OFFSET` progress is forbidden; equal-time records across a batch boundary cannot be skipped or duplicated. The complete composite position is inside `SourceCursor`/`SourceHeadV1` and commits atomically with observations/outbox.
- Truncation, head-fingerprint change before the committed offset, SQLite replacement, or native artifact identity change starts `generation + 1`; old observations remain immutable.
- The final unterminated JSONL line is not acknowledged. Malformed complete records are quarantined and the cursor advances only when the quarantine skeleton and outbox marker commit atomically.
- Duplicates preserve one canonical observation plus duplicate-seen metrics. Late/out-of-order records retain occurred time, ingested time, source position, and `late_by`; capture never rewrites prior order.
- A per-source sequence gap emits `capture.sequence_gap_detected`; later arrival emits `capture.sequence_gap_filled`. The drainer waits only for the configured bounded reorder window and never invents missing records.

“Host installation” in `SourceInstanceId` means the stable host-installation entity identity, never its bundle version, component digest, adapter build, install generation, or capability-snapshot digest. The latter belong only to `SourceAdapterExecutionRefV1`/`ProvenanceV1`. The contract suite ingests one native record under two adapter/bundle generations and requires one `ObservationId`, two distinguishable replay/provenance receipts, and an explicit projector supersession/coverage result rather than a duplicate entity.

### Hook hot path and concurrent-agent contract

```rust
pub struct RawHookObservationDraft {
    pub producer: HookProducerId,
    pub tracedecay_build: TraceDecayBuildRefV1,
    pub provider: ProviderId,
    pub host: HostInstanceId,
    pub session_hint: Option<NativeSessionId>,
    pub agent_hint: Option<NativeAgentId>,
    pub parent_agent_hint: Option<NativeAgentId>,
    pub correlation_hint: Option<PrivacyDomainBoundLocatorDigest>,
    pub event: Unclassified<HookEventV1>,
    pub occurred_at: UtcMicros,
}

pub struct WorkClaimScopeDraft {
    pub repositories: Vec<AliasRef>,
    pub worktrees: Vec<AliasRef>,
    pub refs: Vec<AliasRef>,
    pub pull_requests: Vec<AliasRef>,
    pub files: Vec<ClassifiedLocator>,
    pub symbols: Vec<AliasRef>,
    pub query_scope: Option<QueryId>,
}

pub struct WorkClaimDraft {
    pub native_claim_id: Option<NativeEventLocatorDigest>,
    pub goal_hint: Option<AliasRef>,
    pub scope: WorkClaimScopeDraft,
    pub intent: WorkIntent,
    // Pre-sanitizer candidate text; only the sanitizer validates it into
    // `SafeCoordinationSummary` after scanning.
    pub summary: Option<ProviderFieldValue>,
    pub retrieval_anchors: Vec<RetrievalAnchorId>,
    pub redundancy: RedundancyMode,
    pub status: WorkClaimStatus,
    pub expires_at: UtcMicros,
}

pub enum HookEventV1 {
    SessionStarted { source: NativeKindCode },
    SetupStarted { trigger: NativeKindCode },
    InstructionsLoaded { load_reason: NativeKindCode, metadata: ProviderFieldValue },
    PromptSubmitted { prompt_id: Option<AliasRef>, content: ProviderFieldValue },
    PromptExpanded { prompt_id: Option<AliasRef>, expansion: ProviderFieldValue },
    AssistantMessageDisplayed { turn_id: AliasRef, message_id: AliasRef, index: u32, final_chunk: bool, delta: ProviderFieldValue },
    AgentSpawned { child: NativeAgentId, task: ProviderFieldValue },
    AgentStopped { child: NativeAgentId, stop_hook_active: bool, last_message: Option<ProviderFieldValue>, background_tasks: Option<ProviderFieldValue>, session_crons: Option<ProviderFieldValue>, terminal_coverage: CoverageReportV1 },
    AgentMessage { recipient: NativeAgentId, content: ProviderFieldValue },
    AgentHandoff { recipient: NativeAgentId, state: ProviderFieldValue },
    AgentPresenceHeartbeat { status: PresenceStatus },
    WorkClaimDeclared { claim: WorkClaimDraft },
    WorkClaimScopeChanged { claim_id: String, scope: WorkClaimScopeDraft },
    WorkClaimAcknowledged { claim_id: String, redundancy: RedundancyMode },
    CoordinationOutcomeObserved { claim_id: String, outcome: CoordinationOutcome },
    PermissionRequested { prompt_id: Option<AliasRef>, native_request_id: Option<AliasRef>, tool: String, input: ProviderFieldValue },
    PermissionDecisionObserved { permission_request: AliasRef, behavior: PermissionBehaviorV1 },
    PermissionDenied { prompt_id: Option<AliasRef>, tool_use_id: Option<AliasRef>, tool: String, input: ProviderFieldValue, reason: ProviderFieldValue },
    ToolStarted { call_id: String, tool: String, input: ProviderFieldValue },
    ToolFinished { call_id: String, outcome: ToolOutcome, output: ProviderFieldValue, duration_ms: Option<u64> },
    ToolFailed { call_id: String, tool: String, error: ProviderFieldValue, is_interrupt: Option<bool>, duration_ms: Option<u64> },
    ToolBatchFinished { prompt_id: Option<AliasRef>, results: ProviderFieldValue },
    NotificationObserved { kind: NativeKindCode, message: ProviderFieldValue },
    TaskCreated { task: ProviderFieldValue },
    TaskCompleted { task: ProviderFieldValue },
    TeammateIdle { teammate: ProviderFieldValue },
    ConfigurationChanged { source: NativeKindCode, change: ProviderFieldValue },
    CwdChanged { previous: ProviderFieldValue, current: ProviderFieldValue },
    FileChanged { file: ProviderFieldValue, change: ProviderFieldValue },
    WorktreeCreateRequested { request: ProviderFieldValue },
    WorktreeCreated { worktree: ProviderFieldValue },
    WorktreeRemoved { worktree: ProviderFieldValue },
    CompactStarted { trigger: NativeKindCode, custom_instructions: Option<ProviderFieldValue> },
    CompactFinished { trigger: NativeKindCode, compact_summary: Option<ProviderFieldValue> },
    TurnStopRequested { prompt_id: Option<AliasRef>, stop_hook_active: bool, last_message: Option<ProviderFieldValue>, background_tasks: Option<ProviderFieldValue>, session_crons: Option<ProviderFieldValue>, terminal_coverage: CoverageReportV1 },
    TurnStopFailed { prompt_id: Option<AliasRef>, error_type: NativeKindCode, error: ProviderFieldValue },
    ElicitationRequested { server: AliasRef, request: ProviderFieldValue },
    ElicitationAnswered { server: AliasRef, response: ProviderFieldValue },
    ContinuationDecisionObserved { target: HookContinuationTargetV1, continued: bool, reason: Option<ProviderFieldValue> },
    HookHandlerRunObserved { definition: HookDefinitionRefV1, run: HookHandlerRunRefV1, result: HookHandlerResultV1 },
    HintTerminal { hint_id: String, terminal: HintTerminalState },
    SessionStopped { outcome: Option<String>, reason: Option<NativeKindCode> },
}

pub struct HookSpool;

impl HookSpool {
    pub fn append(
        &self,
        observation: &SanitizedHookObservation,
        deadline: std::time::Instant,
    ) -> Result<tracedecay_domain::SpoolReceipt, HookSpoolError>;
    pub fn acknowledge(&self, ack: HookAck) -> Result<(), HookSpoolError>;
    pub fn recover(&self) -> Result<SpoolRecoveryReport, HookSpoolError>;
}
```

Every TraceDecay-owned source adapter and hook draft carries the originating `TraceDecayBuildRefV1`; capture rejects a newly emitted log/hook/diagnostic record without it. Spool frames authenticate the producer build reference in their header, and drain/forward/import preserves it independently from the current drainer/collector build. This applies even before project/store initialization and during recovery failures. Pre-contract V1 JSONL/file logs enter migration as `KnownVersion` when component+SemVer are proven without a build manifest, otherwise explicit `UnknownLegacy`; neither is ever relabeled as the importing binary.

Codex lowering preserves the exact parent-session `session_id`, Turn, agent, tool-use, hook-definition binding, matcher-group, handler, run/attempt, bundle, trust/source-layer evidence, producer-build, and optional collector-build identities available at that surface. `PermissionRequest` has no mandatory native call ID: capture accepts an optional native alias, while application persists a deterministic request identity over session/Turn/tool/sanitized-input digest and source generation. It appends every concurrently launched observable TraceDecay handler run; an invocation-group projection relates them and records host aggregation evidence separately. Shared definition/run/result/trust/source refs are owned by `tracedecay-domain`; capture imports them and never depends on root hook/config composition. `transcript_path`, `agent_transcript_path`, cwd, prompt, tool input/response, and last assistant message remain transient unclassified inputs and may persist only as sanitized payload refs or privacy-domain locator fingerprints. Stop/SubagentStop terminal observations asynchronously dirty only their exact thread/subagent automation scope; the hook never waits for reflection/curation, and unchanged terminal inputs remain fenced by plans 09/26.

Claude lowering preserves the native 30-event identity, only the native correlation fields actually supplied (`prompt_id`, `tool_use_id`, MessageDisplay `turn_id`, and event-specific IDs), conditional effort, session/agent/task/team/tool/batch/worktree/MCP identities, event-specific duration/failure/interrupt/continuation fields, handler kind/execution mode, configured-definition versus host-deduped versus actual-run evidence, and produced-at versus later-delivered context time. It never manufactures a Turn ID from session/timing/text. `transcript_path` is explicitly lagging and never used to infer current-turn completeness. `MessageDisplay` is metadata-only by default—delta text is discarded after sanitizer classification unless a versioned bounded capture purpose passes privacy/performance evaluation. Version-gated background-task/session-cron fields are optional with coverage; missing/unreachable evidence cannot satisfy the Stop/SubagentStop terminal predicate. `StopFailure` remains a distinct non-controllable event.

Capture owns the one hook spool and its drainer. There is exactly one spool implementation, one hash-chained frame format (below), and one always-spool ingress protocol; the store exposes only append transactions and never runs a handoff-first or fallback ingress spool of its own ([`02-store-crate.md`](02-store-crate.md) drains capture's spool through `ObservationJournal` appends). Plan [`07-hooks-crate.md`](07-hooks-crate.md) hook hosts write exclusively through capture's spool client (`spool/client.rs`) and receive durability acks carrying the domain `SpoolReceipt` from [`01-domain-crate.md`](01-domain-crate.md); no crate mints a spool-receipt variant.

Under dedicated-service isolation, that client is an authenticated connect-only call to a socket-activated, service-owned capture-ingress helper that validates the already-sanitized wrapper, sanitizer receipt, binding, header, and digests and then `fdatasync`s the canonical capture spool without opening an application store. The ingress helper never accepts raw/unclassified content and never runs a second sanitizer. It stays available while the main daemon is stopped or draining and hands segments to the normal daemon drainer after restart. Client hooks never receive spool paths or keys and never create a second user-owned spool; if the ingress service itself is unavailable, the hook reports a non-content degraded receipt and never claims durability. Remote-authority mode uses the same local service-owned ingress spool until a verified remote commit receipt retires the frame.

`RawHookObservationDraft` exists only in adapter memory. Plan 07's wire shape `Unclassified<RawHookRequestV1>` decodes one-to-one into `RawHookObservationDraft` at the capture client boundary; no second pre-sanitizer hook shape exists. The hook adapter parses and sanitizes it through the same `ObservationSanitizer` before constructing `SanitizedHookObservation`; only that sanitized wrapper can serialize into a spool frame. A scanner timeout or unavailable privacy policy fails closed with no content retention: it produces a non-content receipt and no hint/content frame, and no encrypted or deferred-scan copy of the input is spooled anywhere outside the store's isolated protected-quarantine service. This fail-closed no-content-retention rule is the canonical statement for the plan set; Plan 18's hook target restates it. It is the mandatory-security tradeoff defined by Plan 18, not a provider-specific fast-path bypass.

- The producer lane is `(profile, host, provider, native session, native agent, process nonce)`. One locked lane allocator assigns a monotonic `sequence`; unrelated agents never share a lock.
- Each append writes a length-delimited AEAD frame with version, producer/node/source, node epoch, sequence, payload length/digest, previous-frame hash, schema/privacy/placement versions, encryption-key epoch, random nonce, and authenticated header to a private segment, then calls `fdatasync` before returning the domain `SpoolReceipt`; a successful receipt is the `Durable` ack. CRC32 may diagnose torn framing before decryption but is never integrity authority. Keys come from the profile credential/key service, rotate by epoch, and remain available through the maximum unacknowledged-spool horizon.
- The 10 ms deadline bounds synchronous lock/flush time. Contention rotates to a unique pending segment via atomic create; it does not wait on the main lane. Disk-full/permission failures return `HookSpoolError::Unavailable` to the hook adapter and emit a visible stderr/host diagnostic; they are never reported as captured.
- Backpressure thresholds are 64 MiB per producer and 2 GiB per profile by default. Crossing the soft threshold still appends durably but flags `DeferredBackpressure` on the receipt and wakes the drainer; crossing the hard threshold rejects content-bearing frames but reserves a 1 MiB metadata lane for one `capture.spool_overflow` marker per producer/hour.
- The drainer verifies the hash chain and CRC, merges lanes by `(occurred_at, producer, sequence)` only for display, commits each producer sequence independently, and writes contiguous acks only after the observation/outbox commit.
- Ack durability uses the store's spool-acknowledgement port with one row shape, `SpoolAckRecordV1 { producer_lane: ProducerLaneId, segment_id: SpoolSegmentId, contiguous_sequence: u64, drainer_lease_epoch: u64, acked_at: UtcMicros }`: primary key `(producer_lane, segment_id)`, compare-and-set on `drainer_lease_epoch`, index on `acked_at` for grace-period compaction, owned by the profile activity shard, and retained only until its segment is deleted after the 24-hour grace.
- Segment deletion requires every sequence in the segment to be durably acknowledged plus a 24-hour recovery grace. Multiple drainers use leases and compare-and-set acks; duplicate reads are harmless.
- In remote-authority mode, “durably acknowledged” means a verified authority `SyncReceiptV1` persisted locally after canonical commit. A connection-level response, upload completion, cache write, or replica receipt cannot retire a frame. Crash after remote commit but before local receipt persistence resends and dedupes.
- Each frame binds node/source stream, monotonic sequence, previous digest, deterministic observation ID/digest, schema/privacy versions, and resolved destination placement/authority epoch. Policy is re-resolved before upload; tightening may quarantine or retain a frame locally rather than leaking it.
- Network loss never blocks append. The drainer uses bounded batches/backoff, reports oldest pending age/bytes/gaps, and distinguishes `pending`, `rejected_policy`, `revoked`, `schema_skew`, `placement_changed`, `identity_collision`, and `authority_unavailable`; no class is silently dropped or retried forever.
- Parent/child, inter-agent, tool, goal, and hint relationships remain hints in observations. Projectors establish provider-declared or evidence-bearing relations; capture does not infer them from timing.

### Provider-native graph evidence

Capture preserves the source vocabulary needed to build the product's graph of graphs; it does not flatten provider events into generic messages before projection.

```rust
pub enum AgentActivityDraft {
    ThreadObserved { native_thread_id: String },
    SessionObserved { native_session_id: String, native_thread_id: Option<String> },
    TurnStarted { native_turn_id: Option<String>, ordinal: Option<u64> },
    TurnContent { native_turn_id: Option<String>, content_kind: NativeContentKind },
    WorkflowRunObserved { native_run_id: String, native_kind: String, status: String },
    AgentSpawned { native_agent_id: String, parent_agent_id: Option<String> },
    AgentMessage { sender: String, recipient: String },
    AgentHandoff { sender: String, recipient: String },
    GoalObserved { native_goal_id: String, native_kind: String, status: String },
    PresenceObserved { status: PresenceStatus, expires_at: UtcMicros },
    WorkClaimObserved {
        native_claim_id: Option<String>,
        scope: WorkClaimScopeDraft,
        intent: WorkIntent,
        summary: Option<ProviderFieldValue>,
        retrieval_anchors: Vec<RetrievalAnchorId>,
        redundancy: RedundancyMode,
        status: WorkClaimStatus,
        expires_at: UtcMicros,
    },
    FileObserved { path: String, operation: String },
    GitObserved { native_object_id: String, native_kind: String },
    MemoryObserved { native_memory_id: String, native_kind: String },
    LegacyCurationObserved { native_artifact_id: String, native_kind: String, status: String },
    CurationCandidateObserved { native_artifact_id: String, status: String },
    AutonomyDecisionObserved { native_artifact_id: String, decision: String },
    AutonomousEffectObserved { native_artifact_id: String, status: String },
    CurationOutcomeObserved { native_artifact_id: String, status: String },
    AutomaticRecoveryObserved { native_artifact_id: String, status: String },
}
```

- Every draft retains provider-native IDs, kind/status strings, ordinal/sequence, and source provenance alongside the canonical payload discriminator.
- Project/repository/checkout/worktree/ref/PR/file/symbol/query evidence is always a zero-to-many candidate set with source field/record provenance. Capture never writes a primary project from `sessions.project_key`, first CWD, current process CWD, active base checkout, current branch, or registry first-match.
- A Turn source record may reference messages/content parts, provider-exposed reasoning summaries, tool invocations/results, files, goals, and usage. Capture records those references but does not create the canonical Turn or its edges.
- Claude workflow/run/roster/journal semantics remain `WorkflowRunObserved` records with their native status and agent IDs; they are not coerced into Codex goal states.
- Codex goal create/update/complete/blocked events retain native goal ID, objective, status, budget, and event type; they are not reduced to workflow-run status.
- Hermes host/user/automation actor hints and curation/self-improvement records preserve historical proposal/validation/approval/apply kinds as `LegacyCurationObserved`, while V2 emits candidate/autonomy-decision/automatic-effect/outcome/recovery observations. Actor or outcome attribution remains a projector decision backed by these observations; capture never turns a legacy approval into a V2 gate.
- Presence/work-claim drafts preserve agent/session/parent/goal aliases; repository/worktree/ref/PR/file/symbol/query scope; read/write intent; an optional summary candidate that only the sanitizer validates into `SafeCoordinationSummary`; retrieval anchors; heartbeat/TTL/status; and declared redundancy mode. Capture never infers material overlap, cancels work, or copies raw task/prompt text into the summary.
- File/Git/memory links retain exact tool/event/source references so projectors can cross-link Turn graphs to timeline, code snapshots, worktrees/commits/PRs, facts/retrieval, and automation without temporal guessing.

### Privacy, reasoning, quarantine, and replay

```rust
pub enum ReasoningArtifactFormat {
    Summary,
    AnalysisText,
    Structured,
    Encrypted,
    Unavailable,
}

pub struct ReasoningArtifactDraft {
    pub format: ReasoningArtifactFormat,
    pub visibility: ProviderVisibility,
    pub content: Option<ProviderFieldValue>,
    pub provider_digest: Option<KeyedSourceRecordFingerprint>,
    pub unavailable_reason: Option<UnavailableReason>,
}

pub struct CaptureReplayManifestV1 {
    pub mode: tracedecay_domain::ReplayMode,
    pub source_artifacts: Vec<ManifestSource>,
    pub observation_ids: Vec<ObservationId>,
    pub parser_artifact_digest: ManifestDigest,
    pub parser_config_digest: ManifestDigest,
    pub privacy_policy_digest: PrivacyPolicyDigest,
    pub detector_set_digest: DetectorSetDigest,
    pub sanitization_receipts_digest: ManifestDigest,
    pub provider_schema_versions: Vec<ProviderSchemaVersion>,
    pub evaluator_bundle_digest: Option<ManifestDigest>,
    pub index_watermarks: Vec<ManifestWatermark>,
    pub memory_manifest_digest: Option<ManifestDigest>,
    pub tool_catalog: Option<CatalogSnapshotRefV1>,
    pub substitutions: BoundedVec<tracedecay_domain::ReplaySubstitutionV1, 64>,
    pub unavailable_inputs: BoundedVec<tracedecay_domain::ReplayUnavailableInputV1, 64>,
}
```

- `Summary`, `AnalysisText`, and `Structured` content is accepted only when the provider delivered it to the host/user. `Encrypted` records store provider metadata/digest and no decrypted text. `Unavailable` is an explicit coverage marker.
- Reasoning defaults to 30-day retention and is excluded from FTS, vectors, facts, shares, and exports. Capture sets policy metadata; downstream stores enforce it.
- Secret-like content is sanitized before the envelope. When explicit policy permits forensic inspection, the store's separate protected-quarantine service first stages encrypted transient bytes under a random `ProtectedSecretRef` with a 24-hour expiry and one-use attachment token. The observation envelope itself carries only a safe marker/receipt, broad reason class, and coverage—never spans, length, prefix/suffix, or candidate digest. The append transaction consumes the attachment token into the non-content `quarantined_writes.protected_secret_ref` skeleton; retry is idempotent by ref/token. A crash or rejected append leaves only an unattached encrypted staging object, which the protected service securely retires after a short grace and never indexes or returns through general reads. This store-internal attachment path is the single reviewed persistence channel for `ProtectedSecretRef`, which otherwise implements no `Display` or public `Serialize`; only the append transaction advances the source head.
- Exact replay is enabled only when every authorized source slice and the executable parser/config/privacy-policy/detector artifacts and sanitization receipts match their digests. Recorded-result mode exposes stored sanitized observations when executable artifacts are unavailable. Best-effort mode lists every substitution and nondeterministic dependency; it cannot claim byte equality or rehydrate provider-owned raw content.
- Quarantine reason codes are a closed enum fixed at ten in versioned revision 2: `malformed_record`, `unsupported_schema`, `invalid_utf8`, `secret_like`, `payload_hash_mismatch`, `source_gap`, `spool_corrupt`, `future_version`, `ownership_conflict`, and `identity_collision` (revision 2 adds `identity_collision` for a same-position digest conflict against an already-committed observation identity). This enum grows only by recorded versioned revision here; [`02-store-crate.md`](02-store-crate.md) cites these codes and mints no store-local reason.

## V1 seam map and ownership

| V1 seam | Capture adapter | V2 ownership/result |
|---|---|---|
| `src/sessions/source.rs::{TranscriptSource, stream_new_jsonl, read_changed_file}` | Shared source/identity/runner contracts | Read-only source framing; V2 cursor advances only with journal commit. |
| `src/sessions/mod.rs::{ingest_global_sources, ingest_global_sources_for_provider}` | Adapter registry and root composition | Provider fan-out; no provider switch in the runner. |
| `src/sessions/codex.rs`, `src/sessions/codex/events.rs`, `src/sessions/codex_app_server.rs` | `adapters/codex.rs` | Codex messages, response-item tools/results, exposed reasoning summaries, goals/plan updates, turn context/usage. |
| `src/sessions/claude.rs` | `adapters/claude.rs` | Messages, exposed thinking, redaction markers, hook/system markers, compact/model fallback, PR link, subagent hints. |
| `src/sessions/cursor.rs`, `cursor_agent.rs`, `cursor_composer.rs` | Cursor and Composer adapters | Agent/composer messages, plans, tools, dispatch, subagents, Git/project candidates. |
| `src/sessions/{cline_like,hermes,kiro,vibe}.rs` | Matching adapters | Existing supported transcript families and provider-native metadata. |
| `src/global_db.rs::{ParseOffset, TranscriptBatch}`, V1 sessions/messages/analytics | `adapters/v1_sessions.rs` | Backfill observations only; canonical transcript ownership moves to profile `activity.db`. |
| `src/sessions/lcm/{raw,schema,dag,compression,payload,gc}.rs` | `adapters/lcm_v1.rs` | Raw/source/summary/compression/payload/tombstone lineage observations; canonical content is not copied into project shards. |
| `src/sessions/git_correlation.rs`, `src/daemon/git_watch.rs` | `adapters/git.rs` | Repository/worktree/ref/commit observations; correlation remains a projector responsibility. |
| `src/sessions/{workflow_ingest,workflow_index,workflow_state}.rs` | Provider and automation adapters | Claude/native workflow run, roster, parent/subagent, agent status, result, and handoff evidence. |
| `src/hooks/{codex,claude,cursor,kiro,analytics,hint_outcomes}.rs` | Hook spool and `hook_events.rs` | High-volume activity observations, exact terminal hint states, per-producer ordering, outcome evidence. |
| `src/automation/{config,scheduler,runner,run_ledger,artifact_payloads,managed_skills,outcomes}.rs` | `adapters/automation.rs` | Config/schedule/lock/skip/run/Hermes actor/artifact/proposal/validation/approval/apply/skill/fact/curation/outcome observations. |

Canonical provider activity, including generic and cross-project sessions, belongs to profile `activity.db`. Project attribution is zero-to-many evidence produced later; project shards receive locators and scoped projections, never duplicate message bodies. Profile/zero-project/cross-project knowledge, skills, policies, and automation also resolve to activity ownership. Project-native Git/code and explicitly project-scoped knowledge/policy/automation evidence belongs to the canonical repository/privacy-domain `project.db`.

Merged PR #405 (`legacy-store-adoption`) is a required pre-backfill seam: source discovery consumes its manifest-backed adopted identity, treats pristine retargeting as the same source, and quarantines nonempty split-identity conflicts instead of minting duplicate artifact IDs. Merged PR #407 keeps `~/.hermes` source-only under the ordinary user profile. Merged #410 remains a semantic fixture: every copied parent/subagent prompt, direct-user row, tool result, and protocol row is captured losslessly. Merged #412/#432 supply lifecycle drain/early-hook deferral evidence; merged #411 supplies foreign skill-owner/remediation events. Accepted inputs through #425 contribute release, identity, routing, catalog-generation, retrieval-event, accounting, and split-store consolidation evidence; #426 preserves untracked branch graphs, #428 preserves divergent session variants, #430 bounds family lookup, #434 fences registry reconstruction, #435 separates search from repair, #436 supplies peer-checkpoint fixtures, and #438 retirement never becomes capture-side self-healing. Merged #447/#448 supply scan-once, semantic-frame/cursor, selected-profile/live-hook priority, and truthful refresh differentials. The conformance manifest records actual merge/base/open commits and semantics after a live refresh.

## Per-provider conformance matrix

| Adapter | Required fixture assertions |
|---|---|
| Codex | Session metadata; turn CWD/Git updates; response-item call/output/tool-search/web-search; provider-exposed `reasoning.summary`; create/update/complete/blocked goal events; compacted summaries; usage; malformed/partial JSONL; app-server events. |
| Copied-prompt origin | PR #410 eight-child fixture; preserve every native row and parent/child locator; prove capture performs no irreversible representative dedupe. |
| Claude | Human/assistant/protocol role distinction; tool use/result; exposed thinking separated from visible message; redacted-only/encrypted marker without plaintext; PR/compact/model-fallback markers; parent/subagent IDs and parent tool-use ID; all CWD/worktree candidates over time; prove first CWD is not canonical attribution. |
| Cursor agent | Project/CWD candidates; timestamp carry; model; tool dispatch/result; parent/subagent transcript discovery; agent dispatch target; late/out-of-order records. |
| Cursor Composer | Read-only SQLite/envelope/blob discovery; bubble order; plans; tool/edit metadata; PR/Git metadata; replacement database rewrite generation. |
| Cline-like | Provider identity, message/tool families, source ordering, malformed record quarantine, unknown fields preserved in forensic payload. |
| Hermes | Transcript source under `~/.hermes`; ordinary user-profile ownership; zero/one/many host-profile source partitions; one source open/sweep per committed frontier; provider-filtered discovery; canonical activity observation with zero-to-many later project attributions; skewed destination visibility never causes source rescan; migrated session/fact collision and idempotent-ledger fixtures from PR #407; no Hermes-only runtime store route. |
| Kiro | Transcript messages, hook records, tool/result, project hints, partial line and rewrite behavior. |
| Vibe | Session metadata, message ordering, usage metadata, changed-file cursor, missing timestamp reason. |
| Hook stream | Codex/Claude/Cursor/Kiro event taxonomy; per-producer sequence; parent/child/inter-agent messages; duplicate/gap/fill/late markers; hint terminal/outcome linkage. |
| Coordination | Presence/claim/heartbeat/scope/ack/handoff events; every redundancy mode; safe-summary and anchor privacy; same and parallel worktrees; TTL expiry source evidence; current-parent prefix `019f4906` resolved to its unique full session ID; PR #359 duplicate-review children `agent-ac3ce9b1ebf998cfb`, `agent-a245d2442cefc621d`, `agent-a96d21dc6391ceba8`, `agent-a6661fd133491631c`; shared-worktree Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d`. |
| V1 LCM | Raw/source/summary DAG hashes and ranges; payload references; compression boundary/decision; lifecycle/tombstone; redaction and missing payload quarantine. |
| V1 automation | Config source; schedule/lock/skip; run events; roster agents; artifacts and hashes; proposals/approvals; skill versions; fact/skill outcomes. |
| Code snapshot | Tracked-file framing at explicit repository/checkout/worktree/ref/snapshot tuples; bounded dirty overlays; large-blob/binary/generated-file scan budgets with explicit skip coverage; secret-bearing repository fixtures proving sanitizer conformance and zero plaintext leakage; rewrite generation on checkout/ref switch; deterministic snapshot manifest hashes consumed by the plan 25 indexer. |

All provider fixtures assert the normalized envelope JSON, source key/generation/position/hash, sensitivity/retention, replay manifest, and second-ingest result of zero inserted observations.

The `code_snapshot` adapter is the single sanctioned sanitizer-crossing entry point for repository text: repo content flows `code_snapshot` adapter → capture sanitizer → sanitized observations → [`25-code-intelligence-indexing-crate.md`](25-code-intelligence-indexing-crate.md) indexer → plan 02 graph generations → plan 05 queries. No indexer, watcher, or snippet/label/embedding builder reads repository files around capture.

## PR and task sequence

Plan 03 is authoritative for capture slicing: the former master-plan PR 7 harness/bootstrap work is consolidated into PR 7A so the mandatory sanitizer and receipt types exist before the journal runner, shadow capture, or any V2 observation comparison. There is no separate implementation PR 7.

### PR 7A: Crate contracts, mandatory sanitizer, deterministic identity, and journal runner

**Files:** create `Cargo.toml`, `src/{lib,error,source,identity,normalize,journal,runner,quarantine,replay}.rs`, the exact `src/privacy/**` tree from Plan 18, `tests/{contract_suite,privacy_security}.rs`; modify workspace `Cargo.toml`.

- [ ] Write failing tests named `same_record_has_same_observation_id`, `adapter_generation_changes_provenance_not_observation_id`, `host_capability_snapshot_must_match_runtime`, `key_rotation_keeps_observation_id_and_requires_continuity_receipt`, `append_growth_keeps_generation`, `rewrite_increments_generation`, `partial_line_does_not_advance_cursor`, `journal_commit_is_idempotent`, `quarantine_advances_atomically`, `unclassified_cannot_serialize_or_enter_sink`, `complete_receipt_required_for_observation`, `serialized_fields_scan_independently`, `scan_failure_commits_skeleton_not_content`, `exact_replay_rejects_digest_substitution`, `capture_preserves_multi_repo_worktree_generation_scope`, `empty_scope_is_not_current_project`, and `scope_candidates_never_replace_requested_scope`.
- [ ] Add the public signatures above and exhaustive enums with serde tags fixed to `snake_case`.
- [ ] Implement canonical identity bytes, compare-and-set source state, record framing, Plan 18's parse-before-scan engine/policy/receipts/bounded detector registry, replay manifests, and runner retry semantics. Make sanitized observation the only journal/spool input; retire message-metadata opt-out semantics.
- [ ] Add architecture lint that rejects imports matching `tracedecay::sessions`, `tracedecay::hooks`, `tracedecay::automation`, `mcp`, or `dashboard` from the crate.
- [ ] Run `cargo test -p tracedecay-capture --test contract_suite`; expected: exit 0 and all seventeen named contracts pass.
- [ ] Run `cargo clippy -p tracedecay-capture --all-targets --all-features -- -D warnings`; expected: exit 0 with no warnings.
- [ ] Commit `feat(capture): add deterministic observation runner`.

### PR 7B: Durable hook spool and concurrent-agent capture

**Files:** create `src/spool/{mod,client,frame,recovery}.rs`, `src/hook.rs`, `tests/hook_spool_suite.rs`, `benches/capture.rs`.

- [ ] Write failing tests for 128 concurrent producer lanes, same-lane monotonic sequence, duplicate drain, sequence gap/fill, out-of-order occurred time, crash before/after `fdatasync`, crash before/after journal ack, corrupt tail truncation, `0600` O_EXCL fallback/lock/JSONL writes, symlink rejection, overflow marker reservation, and two competing drainers.
- [ ] Add remote-mode cases for AEAD/key rotation, partition/reconnect, placement/policy/grant change, duplicate/reordered upload, authority commit before response, response before local receipt persistence, signed-receipt expiry/replay/revocation, and tombstone-before-cache-serve.
- [ ] Implement AEAD framed hash-chained segments, pending-lane rotation, contiguous local or signed-authority acks, lease/CAS drain, recovery scan, soft/hard backpressure, and diagnostics stated above. Store PR 6H persists acknowledgement/cache metadata but never implements or decrypts the spool.
- [ ] Assert parent/child/inter-agent/tool/hint fields survive spool/recovery as byte-identical sanitized structures with receipt bindings and are not inferred from process order; raw provider bytes never enter a general spool segment.
- [ ] Run `cargo test -p tracedecay-capture --test hook_spool_suite`; expected: exit 0; recovery yields no lost acknowledged frame and no duplicate observation.
- [ ] Run `cargo bench -p tracedecay-capture --bench capture -- hook_append`; expected: benchmark report records reference machine, concurrency, p50/p95/p99, and p95 at or below 8 ms at 128 producers.
- [ ] Commit `feat(capture): add durable concurrent hook spool`.

### PR 7C: Codex and Claude adapters

**Files:** create `src/adapters/{mod,codex,claude,hook_events}.rs`; add redacted fixtures under `tests/fixtures/v2/providers/{codex,claude,hooks}/`; extend `tests/provider_conformance.rs`.

- [ ] Port source semantics from the exact V1 seams without importing V1 structs; preserve unknown provider fields only in protected forensic payloads.
- [ ] Add fixtures for every Codex/Claude row in the conformance matrix, including tools, goals, parent/subagents, presence/work claims/redundancy, hook events, visible reasoning, encrypted/redacted markers, rewrites, partial records, and secrets. Freeze the current-parent prefix and four PR #359 child anchors in the coordination manifest.
- [ ] Assert no developer/system boilerplate becomes a conversational message and no encrypted/hidden reasoning becomes plaintext.
- [ ] Run `cargo test -p tracedecay-capture --test provider_conformance codex`; expected: exit 0 and fixture manifest hashes match.
- [ ] Run `cargo test -p tracedecay-capture --test provider_conformance claude`; expected: exit 0 and fixture manifest hashes match.
- [ ] Commit `feat(capture): conform codex and claude sources`.

### PR 7D: Cursor family and remaining provider adapters

**Files:** create `src/adapters/{cursor,cursor_composer,cline_like,hermes,kiro,vibe,code_snapshot}.rs`; add matching fixture directories; extend `tests/provider_conformance.rs`.

- [ ] Implement Cursor agent/Composer read-only framing, dispatch/subagent/presence/claim evidence, SQLite replacement detection, and bounded blob traversal; include shared-worktree session `ebc96a27-b046-4c88-865f-b38d76da9d2d`.
- [ ] Implement Cline-like, Hermes, Kiro, and Vibe adapters with every matrix assertion.
- [ ] Implement the `code_snapshot` extractor adapter with explicit repository/checkout/worktree/ref/snapshot tuple identity, bounded dirty overlays, large-blob/binary budgets with skip coverage, and secret-bearing repository fixtures proving sanitizer conformance for the plan 25 pipeline.
- [ ] Regenerate the Hermes fixture manifest from merged PR #407 and prove `~/.hermes` is source-only while sessions/LCM are activity-owned and scope-sensitive histories retain `DeclaredScope` for activity/project routing.
- [ ] Add Hermes scale/fault fixtures that open the provider source once for 30 registered projects, route one sanitized canonical stream to zero-to-many attribution projections, preserve independent source and projection checkpoints under skew, keep later valid rows ingestible after a quarantined malformed middle row, resume after cancellation/partial projector failure without rescanning committed input, and make the second refresh add zero observations.
- [ ] Add FM-158 boundary/generation fixtures: run every split point through multi-row Hermes turns, including routing/tool evidence immediately after rows 1,999/2,000/2,001, and require chunk-size-invariant observations/attribution. Advance only through the last complete semantic frame. Treat negative/nonmonotonic/overflow native positions, truncation, and replacement fingerprints as generation evidence; never cast signed positions into an unsigned cursor or advance an ambiguous legacy import.
- [ ] Run `cargo test -p tracedecay-capture --test provider_conformance`; expected: exit 0 for every adapter registered in `adapters/mod.rs` and no untested registry entry.
- [ ] Run `cargo test --test transcript_ingest_suite`; expected: existing V1 provider suite remains green because shadow capture does not change V1 writes.
- [ ] Commit `feat(capture): conform remaining provider sources`.

### PR 7E: V1 LCM, Git, sessions, hooks, and automation backfill adapters

**Files:** create `src/adapters/{lcm_v1,git,automation,v1_sessions}.rs`; add copied-store fixture manifests; extend `tests/provider_conformance.rs` and `tests/shadow_parity.rs`.

PR 7E owns V1 parse and sanitize: every byte of V1 import content passes the mandatory sanitizer here and produces `SanitizationReceiptV1` records before any batch leaves capture. The storage-side transaction executor that consumes these sanitized batches is plan 02's PR 33S importer, which adds no parsing, classification, or redaction of its own; PR 33S-2 is cutover/rollback-window/deletion-proof support, not an importer ([`02-store-crate.md`](02-store-crate.md); [`12-root-compatibility-migration.md`](12-root-compatibility-migration.md) references this split).

- [ ] Capture every LCM raw/summary/source/compression/payload/lifecycle/tombstone family, session/message/analytics row, Git/worktree/ref/commit observation, hook/hint terminal row, and automation family listed in the seam map.
- [ ] Add the provider-global backfill-marker regression: a completed marker for one provider/source artifact cannot suppress scanning another provider or cause every source to reparse. Checkpoints are keyed by `(adapter, source instance, artifact, rewrite generation)` and report per-provider reparsed/skipped counts.
- [ ] On the base containing merged #405, assert moved roots, symlinks, linked worktrees, and pristine adopted stores retain source/artifact identity; nonempty split identities produce `ownership_conflict` quarantine.
- [ ] Refresh from merged PR #407 and assert session/fact/LCM migrations produce one idempotent source lineage with collision reports and `DeclaredScope` preserved.
- [ ] Run `cargo test -p tracedecay-capture --test provider_conformance v1_`; expected: exit 0 and every V1 structured family has at least one golden observation.
- [ ] Run `cargo test -p tracedecay-capture --test shadow_parity backfill_manifest`; expected: counts/hashes/offsets/source lineage/payload refs match or appear in the explicit quarantine report.
- [ ] Commit `feat(capture): add v1 backfill sources`.

### PR 7F: Shadow capture, parity, cutover, and rollback

**Files:** create `src/shadow.rs`; extend `tests/shadow_parity.rs`; modify root composition only in the execution PR after this plan is approved.

- [ ] Persist a migration receipt containing source key, V1 cursor, V2 cursor, freeze watermark, adapter/parser/privacy-policy/detector/receipt digests, inserted/duplicate/sanitized/quarantine/unknown counts, and rollback owner.
- [ ] Dual-read each source while V1 remains authoritative; compare per-provider session/message/tool/reasoning/goal/subagent/LCM/Git/hook/automation counts, privacy-domain-keyed source fingerprints, and sanitized-output/manifest digests.
- [ ] Require zero unexplained parity gaps, no corrupt spool segment, projection lag below two seconds for 24 hours, hook p95 at or below 8 ms, and secret-corpus zero leakage before capture cutover.
- [ ] Cut over source-offset ownership by bounded source family; stop V1 advancement only after the freeze watermark is journaled.
- [ ] Cut over while a source batch ends mid-turn and prove the receipt stops at the prior complete semantic frame. Live hook frames use the bounded priority lane and cannot be delayed, suppressed, or made invisible by a refresh cooldown/backfill; refresh and hook ingestion share source identity without sharing admission priority.
- [ ] Drill rollback by disabling V2 capture, restoring V1 offset ownership from the receipt, draining neither side past the freeze watermark, and proving the next V1 ingest is duplicate-free.
- [ ] Run `cargo test -p tracedecay-capture --test shadow_parity`; expected: exit 0 with a machine-readable zero-unexplained-gap receipt.
- [ ] Run `cargo test --test transcript_ingest_suite --test session_suite --test automation_runner_test --test hooks_lsp_suite`; expected: V1 compatibility suites exit 0.
- [ ] Commit `feat(capture): add shadow cutover and rollback receipts`.

## Compatibility, cutover, and rollback rules

- V1 provider parsing and writes remain authoritative until that source family's receipt is accepted; V2 shadow failures cannot block V1 host operation.
- V1 and V2 capture outputs are compared internally during shadowing, but cutover exposes only the current protocol/catalog surface. Stale CLI/MCP/daemon/plugin/hook clients and retired tool/event names receive an exact version-mismatch/restart/update error; capture never guesses or falls back to a V1 runtime path.
- V1 source files and stores stay read-only-accessible for one release after verified cutover; capture never deletes them.
- Parity compares normalized semantics and source evidence, not only totals. Every difference is `expected_transform`, `redacted`, `quarantined`, `v1_bug_preserved`, or `unexplained`; `unexplained` blocks cutover.
- Rollback does not delete V2 observations. It freezes V2 at the receipt watermark, restores V1 source-offset ownership, and marks subsequent V2 observations as a new capture epoch when shadowing resumes.

## Release gates

### Correctness and recovery

- Second ingest of every fixture and copied store inserts zero observations.
- Kill tests at spool write/flush, blob stage/publish, observation insert, outbox insert, cursor advance, ack write, and segment compaction yield complete commit or safe retry.
- Rewrite, duplicate, late, out-of-order, and gap behavior matches the fixed semantics above.
- Copied real-store manifests reconcile counts, hashes, offsets, timestamps, ordinals, payload hashes, LCM DAG/source lineage, artifact hashes, and quarantine.

### Performance and concurrency

- Hook synchronous capture p95 at or below 8 ms at 128 concurrent producers, fitting plan 07's capture sub-budget inside its 10 ms notification-hook total; p99 and rejected/deferred counts are reported.
- Journal append p95 at or below 20 ms excluding blob I/O.
- Backfill sustained throughput at least 10,000 messages/second excluding embeddings.
- A cold full-history Hermes refresh over the current 30-project corpus completes in ≤60 seconds, opens/scans each provider source once, and reports source-open count, records/bytes read, destination-attribution count, p50/p95 batch latency, peak RSS, cancellation boundary, and joined-request count; the same manifest is also exercised at 10× scale under explicit resource budgets.
- Projected visibility is measured end-to-end by the projector plan and must be at or below two seconds p95 before cutover.
- Spool recovery of 1 million frames completes without loading all payloads into memory; benchmark records peak RSS.

### Privacy

- Committed secret corpus yields zero secret-bearing FTS/vector/fact/fixture/export/log hits.
- Files, spool segments, quarantine blobs, and manifests are private; hash/permission doctor tests pass.
- Reasoning capture is opt-in, provider-exposed only, shorter-retained, and excluded from search/export by default.
- Locked privacy domains expose metadata/coverage only; capture never falls back to plaintext.

### Observability

- Metrics expose discovery, source-open/sweep count, bytes/records scanned, destination-attribution fan-out, scan-amplification ratio, refresh leaders/joiners/cancellations, source generation/cursor, ingest rate/lag, duplicates, rewrites, gaps/fills, late records, spool bytes/oldest age, ack lag, backpressure, errors/quarantine, parser/schema coverage, redactions, and cutover epoch.
- Logs use safe IDs/reason codes and never source literals, hook prompts, tool payloads, reasoning, secrets, or redacted content.
- Every report names profile, source adapter/version, source watermark, searched/skipped/unavailable/incompatible/redacted coverage, and migration receipt.

## Definition of done

- Every adapter in the registry has redacted conformance fixtures, a deterministic manifest, and second-ingest idempotency proof.
- One Plan 18 sanitizer owns all runtime detection/redaction and is the only constructor of `SanitizedObservation`/`SanitizationReceiptV1`; adapters, hooks, V1 LCM, memory, store, and projectors contain no competing redactor or bypass.
- No unclassified provider or hook bytes reach general spool/blob/journal/log/fixture/replay storage; scanner failure leaves only a non-content coverage skeleton and optional isolated protected reference.
- Every run preserves one explicit `ScopeSelectorV2` and reports multi-repo/project/checkout/worktree/ref/snapshot/generation candidates, ambiguity, stale registry evidence, and missing coverage without CWD/`project_key`/first-CWD/base-checkout/current-graph fallback.
- Hook capture remains durable and bounded with many concurrent agents, visible backpressure, and no silent drop.
- V1 sessions, LCM, tools, reasoning markers, goals, subagents, Git, hooks/hints, and automation families are represented as immutable observations with explicit ownership.
- #405 identity adoption, #407 profile consolidation, #410 lossless copied prompts, #411 foreign skill ownership/remediation events, and #412 lifecycle-drain receipts are present in the recorded base and parity fixtures; #413 contributes the actual release/protocol version only.
- Exact, recorded-result, and best-effort manifests never overclaim reproducibility or hidden reasoning availability.
- Capture cutover and rollback drills pass without deleting V1 or duplicating canonical evidence.
