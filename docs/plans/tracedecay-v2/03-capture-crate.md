# TraceDecay V2 Capture Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `tracedecay-capture`, the deterministic, privacy-first boundary that discovers V1 and live provider artifacts, durably spools high-volume hook events, and commits idempotent `ObservationEnvelopeV1` records without owning canonical events or read projections.

**Architecture:** Provider adapters discover, frame, and parse source records into transient `Unclassified` drafts. The single Plan 18 sanitizer classifies structured fields, redacts or drops content, issues `SanitizationReceiptV1`, and creates a `Sanitized` observation before any general spool/blob/journal write. A shared normalizer assigns source identity, rewrite generation, offsets, hashes, privacy/retention, and replay metadata before an `ObservationSink` transaction publishes observations and outbox rows. Hook processes use the same mandatory sanitizer before a bounded append-only spool; asynchronous drainers reuse the same journal path as transcript, Git, LCM, and automation importers.

**Tech Stack:** Rust workspace; `tracedecay-domain` contracts; `serde`/`serde_json`; SHA-256 and UUID namespaced identity through domain helpers; `rusqlite`-backed sink supplied by `tracedecay-store`; private append-only spool segments; property tests, redacted golden fixtures, crash tests, and Criterion benchmarks.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) requires capture of provider-native goals/plans/workflows, executor registration/lifecycle observations, workspace/Git/delivery facts, tool effects, costs, and external task-system records as sanitized evidence. Capture never materializes schedulable work, assigns an executor, grants authority, or treats a provider/board status as canonical completion.

---

## Goals

- Capture every supported V1 provider/source family without changing V1 writes during shadow mode.
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
- No cloud ingestion service, remote transcript upload, or required daemon.
- No parsing of encrypted reasoning payloads and no labeling ordinary assistant text as reasoning.
- No deletion of V1 sources, V1 stores, hook JSONL, or automation files during capture cutover.
- No cross-shard transaction; the sink commits one owning shard and reports its outbox sequence.

## Convergence boundary

Capture is the sole runtime content-ingress/sanitizer owner in [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md) and [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md). It consumes the exact domain taint/scope/evidence types from [`01-domain-crate.md`](01-domain-crate.md), uses store ports from [`02-store-crate.md`](02-store-crate.md), and emits only observations for [`04-projectors-crate.md`](04-projectors-crate.md). Scout/model/delivery evidence from [`22`](22-incremental-context-scout-and-suggestion-envelopes.md) and occurrence/correction/summary evidence required by [`23`](23-session-lcm-temporal-retrieval-and-evaluation.md) enter through the same sanitized observation contract; capture never ranks or addresses them.

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
}

pub struct SourceArtifact {
    pub source_instance: SourceInstanceId,
    pub artifact_id: ArtifactId,
    pub privacy_domain: PrivacyDomainId,
    pub locator: SourceLocator,
    pub identity_fingerprint: [u8; 32],
    pub head_fingerprint: [u8; 32],
    pub observed_len: u64,
    pub observed_modified_at: Option<UtcMicros>,
}

pub struct SourceRecord {
    pub position: SourcePosition,
    pub occurred_at: OccurredAt,
    pub encoding: RecordEncoding,
    pub bytes: Vec<u8>,
}

pub enum SourcePosition {
    ByteOffset { start: u64, end: u64 },
    RowId(i64),
    Sequence(u64),
    ObjectKey(String),
}

pub struct SourceBatch {
    pub generation: RewriteGeneration,
    pub records: Vec<SourceRecord>,
    pub next_cursor: SourceCursor,
    pub completeness: BatchCompleteness,
    pub detected_gaps: Vec<SequenceGap>,
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
    // In-memory routing handle only: persists solely via the store's isolated
    // quarantine skeleton (plan 02 `quarantined_writes`), never in the envelope.
    pub protected: Option<ProtectedSecretRef>,
}
```

```rust
pub trait ObservationSink: Send + Sync {
    fn source_state(&self, key: &SourceKey) -> Result<Option<CommittedSourceState>, CaptureError>;
    fn commit(
        &self,
        expected: Option<&CommittedSourceState>,
        batch: ObservationCommit,
    ) -> Result<CaptureCommitReceipt, CaptureError>;
    fn quarantine(&self, entry: QuarantineEntry) -> Result<QuarantineId, CaptureError>;
}

pub struct ObservationCommit {
    pub source_key: SourceKey,
    pub previous_cursor: SourceCursor,
    pub next_cursor: SourceCursor,
    pub envelopes: Vec<ObservationEnvelopeV1>,
    pub replay_manifest: CaptureReplayManifestV1,
}

pub struct CaptureCommitReceipt {
    pub append: tracedecay_domain::AppendReceipt,
    pub committed_cursor: SourceCursor,
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

`ObservationSink::commit` is compare-and-set on the full previous source state. The store implementation inserts observations with `ON CONFLICT(observation_id) DO NOTHING`, inserts one outbox row for each new observation, and advances the cursor in the same transaction. A crash before commit leaves the previous cursor; a crash after commit returns the existing receipt on retry.

`ObservationSanitizer` is the only implementation permitted to construct `SanitizedObservation` or mint `SanitizationReceiptV1`. Its module layout and detector/plugin/budget semantics are exactly Plan 18 Section 8. Capture mints every receipt; the durable receipt home is the per-shard `sanitization_receipts` table defined in [`02-store-crate.md`](02-store-crate.md) (receipt ID primary key, envelope/observation foreign key, sanitizer/detector versions, taint verdict, expiry/revocation state, observation-ID index), and `ObservationSink::commit` persists each receipt in the same transaction as its envelope. Plan [`04-projectors-crate.md`](04-projectors-crate.md)'s sink firewall validates receipts against that table; [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md) defines the receipt's fields and invariants. Adapters parse and identify structured fields but cannot classify eligibility themselves. `ObservationSink` rejects an envelope whose receipt, output digest, privacy domain, parser/detector/policy digest, or completeness does not match. Incomplete/timeout/unsupported scans commit a non-content coverage/quarantine skeleton; they never commit the draft bytes.

### Deterministic identity, rewrite, offsets, and ordering

```rust
pub struct CaptureObservationIdentity {
    pub source_instance: SourceInstanceId,
    pub artifact_id: ArtifactId,
    pub generation: RewriteGeneration,
    pub position: SourcePosition,
    pub source_fingerprint: KeyedSourceRecordFingerprint,
}

pub fn lower_observation_key(input: &CaptureObservationIdentity) -> tracedecay_domain::ObservationKey;
pub fn detect_rewrite(
    committed: Option<&CommittedSourceState>,
    artifact: &SourceArtifact,
) -> RewriteDecision;
```

`SourceRecord.bytes` and any transient checksum exist only in bounded capture/sanitizer memory and cannot implement `Serialize`, `Display`, logging, repository, or receipt traits. The sanitizer computes `KeyedSourceRecordFingerprint` with the privacy-domain key after parsing/classification; only that fingerprint and the sanitized output digest may enter `CaptureObservationIdentity`, `ObservationKey`, provenance, spools, or stores.

- `SourceInstanceId` is a namespaced deterministic ID over profile, host installation, adapter ID, and provider-native source instance.
- `ArtifactId` is a namespaced deterministic ID over source instance plus the provider-native durable artifact identity; a pathname is only one alias.
- The adapter normalizes the five `CaptureObservationIdentity` fields into the domain `ObservationKey` canonical field encoding; `derive_observation_id` is the only observation-ID implementation. Capture may not define a second UUID namespace or canonical encoder.
- `SourcePosition` persists through the offset-lowering columns defined in [`02-store-crate.md`](02-store-crate.md): `observations`/`source_heads` store `(position_kind TEXT, byte_start INTEGER NULL, byte_end INTEGER NULL, object_key TEXT NULL)` with `contiguous_byte_offset` retained for byte-ordered sources. Plan 02 documents the lowering and per-`SourceOrdering` contiguity; capture treats the lowered columns as opaque storage and round-trips every variant, including `ObjectKey(String)` and `ByteOffset{start,end}`.
- Append growth with matching artifact and head fingerprints preserves the generation and resumes at the committed cursor.
- Truncation, head-fingerprint change before the committed offset, SQLite replacement, or native artifact identity change starts `generation + 1`; old observations remain immutable.
- The final unterminated JSONL line is not acknowledged. Malformed complete records are quarantined and the cursor advances only when the quarantine skeleton and outbox marker commit atomically.
- Duplicates preserve one canonical observation plus duplicate-seen metrics. Late/out-of-order records retain occurred time, ingested time, source position, and `late_by`; capture never rewrites prior order.
- A per-source sequence gap emits `capture.sequence_gap_detected`; later arrival emits `capture.sequence_gap_filled`. The drainer waits only for the configured bounded reorder window and never invents missing records.

### Hook hot path and concurrent-agent contract

```rust
pub struct RawHookObservationDraft {
    pub producer: HookProducerId,
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
    SessionStarted,
    PromptSubmitted { content: ProviderFieldValue },
    AgentSpawned { child: NativeAgentId, task: ProviderFieldValue },
    AgentMessage { recipient: NativeAgentId, content: ProviderFieldValue },
    AgentHandoff { recipient: NativeAgentId, state: ProviderFieldValue },
    AgentPresenceHeartbeat { status: PresenceStatus },
    WorkClaimDeclared { claim: WorkClaimDraft },
    WorkClaimScopeChanged { claim_id: String, scope: WorkClaimScopeDraft },
    WorkClaimAcknowledged { claim_id: String, redundancy: RedundancyMode },
    CoordinationOutcomeObserved { claim_id: String, outcome: CoordinationOutcome },
    ToolStarted { call_id: String, tool: String, input: ProviderFieldValue },
    ToolFinished { call_id: String, outcome: ToolOutcome, output: ProviderFieldValue },
    CompactStarted,
    CompactFinished,
    HintTerminal { hint_id: String, terminal: HintTerminalState },
    SessionStopped { outcome: Option<String> },
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

Capture owns the one hook spool and its drainer. There is exactly one spool implementation, one hash-chained frame format (below), and one always-spool ingress protocol; the store exposes only append transactions and never runs a handoff-first or fallback ingress spool of its own ([`02-store-crate.md`](02-store-crate.md) drains capture's spool through `ObservationJournal` appends). Plan [`07-hooks-crate.md`](07-hooks-crate.md) hook hosts write exclusively through capture's spool client (`spool/client.rs`) and receive durability acks carrying the domain `SpoolReceipt` from [`01-domain-crate.md`](01-domain-crate.md); no crate mints a spool-receipt variant.

`RawHookObservationDraft` exists only in adapter memory. Plan 07's wire shape `Unclassified<RawHookRequestV1>` decodes one-to-one into `RawHookObservationDraft` at the capture client boundary; no second pre-sanitizer hook shape exists. The hook adapter parses and sanitizes it through the same `ObservationSanitizer` before constructing `SanitizedHookObservation`; only that sanitized wrapper can serialize into a spool frame. A scanner timeout or unavailable privacy policy fails closed with no content retention: it produces a non-content receipt and no hint/content frame, and no encrypted or deferred-scan copy of the input is spooled anywhere outside the store's isolated protected-quarantine service. This fail-closed no-content-retention rule is the canonical statement for the plan set; Plan 18's hook target restates it. It is the mandatory-security tradeoff defined by Plan 18, not a provider-specific fast-path bypass.

- The producer lane is `(profile, host, provider, native session, native agent, process nonce)`. One locked lane allocator assigns a monotonic `sequence`; unrelated agents never share a lock.
- Each append writes a length-delimited frame with version, producer, sequence, payload length, CRC32, SHA-256, and previous-frame hash to a private segment, then calls `fdatasync` before returning the domain `SpoolReceipt`; a successful receipt is the `Durable` ack.
- The 10 ms deadline bounds synchronous lock/flush time. Contention rotates to a unique pending segment via atomic create; it does not wait on the main lane. Disk-full/permission failures return `HookSpoolError::Unavailable` to the hook adapter and emit a visible stderr/host diagnostic; they are never reported as captured.
- Backpressure thresholds are 64 MiB per producer and 2 GiB per profile by default. Crossing the soft threshold still appends durably but flags `DeferredBackpressure` on the receipt and wakes the drainer; crossing the hard threshold rejects content-bearing frames but reserves a 1 MiB metadata lane for one `capture.spool_overflow` marker per producer/hour.
- The drainer verifies the hash chain and CRC, merges lanes by `(occurred_at, producer, sequence)` only for display, commits each producer sequence independently, and writes contiguous acks only after the observation/outbox commit.
- Ack durability uses the store's spool-acknowledgement port with one row shape, `SpoolAckRecordV1 { producer_lane: ProducerLaneId, segment_id: SpoolSegmentId, contiguous_sequence: u64, drainer_lease_epoch: u64, acked_at: UtcMicros }`: primary key `(producer_lane, segment_id)`, compare-and-set on `drainer_lease_epoch`, index on `acked_at` for grace-period compaction, owned by the profile activity shard, and retained only until its segment is deleted after the 24-hour grace.
- Segment deletion requires every sequence in the segment to be durably acknowledged plus a 24-hour recovery grace. Multiple drainers use leases and compare-and-set acks; duplicate reads are harmless.
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
    pub provider_digest: Option<[u8; 32]>,
    pub unavailable_reason: Option<UnavailableReason>,
}

pub struct CaptureReplayManifestV1 {
    pub mode: tracedecay_domain::ReplayMode,
    pub source_artifacts: Vec<ManifestSource>,
    pub observation_ids: Vec<ObservationId>,
    pub parser_artifact_digest: [u8; 32],
    pub parser_config_digest: [u8; 32],
    pub privacy_policy_digest: PrivacyPolicyDigest,
    pub detector_set_digest: DetectorSetDigest,
    pub sanitization_receipts_digest: ManifestDigest,
    pub provider_schema_versions: Vec<ProviderSchemaVersion>,
    pub evaluator_bundle_digest: Option<[u8; 32]>,
    pub index_watermarks: Vec<ManifestWatermark>,
    pub memory_manifest_digest: Option<[u8; 32]>,
    pub tool_catalog: Option<CatalogSnapshotRefV1>,
    pub substitutions: Vec<ReplaySubstitution>,
    pub unavailable_inputs: Vec<UnavailableInput>,
}
```

- `Summary`, `AnalysisText`, and `Structured` content is accepted only when the provider delivered it to the host/user. `Encrypted` records store provider metadata/digest and no decrypted text. `Unavailable` is an explicit coverage marker.
- Reasoning defaults to 30-day retention and is excluded from FTS, vectors, facts, shares, and exports. Capture sets policy metadata; downstream stores enforce it.
- Secret-like content is sanitized before the envelope. When explicit policy permits forensic inspection, the store's separate protected-quarantine service encrypts transient bytes under a random `ProtectedSecretRef` with 24-hour expiry. The observation envelope itself carries only a safe marker/receipt, broad reason class, and coverage—never spans, length, prefix/suffix, or candidate digest. The opaque protected reference lives only in the non-content quarantine skeleton row (plan 02's `quarantined_writes.protected_secret_ref`, nullable); that store-internal column, written through `ProtectedQuarantineRepository`, is the single reviewed persistence channel for `ProtectedSecretRef`, which otherwise implements no `Display` or public `Serialize`.
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

Merged PR #405 (`legacy-store-adoption`) is a required pre-backfill seam: source discovery consumes its manifest-backed adopted identity, treats pristine retargeting as the same source, and quarantines nonempty split-identity conflicts instead of minting duplicate artifact IDs. Merged PR #407 keeps `~/.hermes` source-only under the ordinary user profile. Merged #410 remains a semantic fixture: every copied parent/subagent prompt, direct-user row, tool result, and protocol row is captured losslessly. Merged #412 supplies lifecycle drain evidence; merged #411 supplies foreign skill-owner/remediation events. #414/#419 and release PRs #413/#416 add no capture semantics by assumption; merged #415/#417/#420/#422/#423/#424/#425 contribute release, identity, routing, catalog-generation, retrieval-event, accounting, and split-store consolidation evidence. Merged #418 (release v0.0.48) is refreshed before implementation; #417's identity-split visibility is a required discovery/quarantine case. PR #409 remains historical. The conformance manifest records actual merge/base commits and semantics.

## Per-provider conformance matrix

| Adapter | Required fixture assertions |
|---|---|
| Codex | Session metadata; turn CWD/Git updates; response-item call/output/tool-search/web-search; provider-exposed `reasoning.summary`; create/update/complete/blocked goal events; compacted summaries; usage; malformed/partial JSONL; app-server events. |
| Copied-prompt origin | PR #410 eight-child fixture; preserve every native row and parent/child locator; prove capture performs no irreversible representative dedupe. |
| Claude | Human/assistant/protocol role distinction; tool use/result; exposed thinking separated from visible message; redacted-only/encrypted marker without plaintext; PR/compact/model-fallback markers; parent/subagent IDs and parent tool-use ID; all CWD/worktree candidates over time; prove first CWD is not canonical attribution. |
| Cursor agent | Project/CWD candidates; timestamp carry; model; tool dispatch/result; parent/subagent transcript discovery; agent dispatch target; late/out-of-order records. |
| Cursor Composer | Read-only SQLite/envelope/blob discovery; bubble order; plans; tool/edit metadata; PR/Git metadata; replacement database rewrite generation. |
| Cline-like | Provider identity, message/tool families, source ordering, malformed record quarantine, unknown fields preserved in forensic payload. |
| Hermes | Transcript source under `~/.hermes`; ordinary user-profile ownership; migrated session/fact collision and idempotent-ledger fixtures from PR #407; no Hermes-only runtime store route. |
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

### PR 7A: Crate contracts, mandatory sanitizer, deterministic identity, and journal runner

**Files:** create `Cargo.toml`, `src/{lib,error,source,identity,normalize,journal,runner,quarantine,replay}.rs`, the exact `src/privacy/**` tree from Plan 18, `tests/{contract_suite,privacy_security}.rs`; modify workspace `Cargo.toml`.

- [ ] Write failing tests named `same_record_has_same_observation_id`, `append_growth_keeps_generation`, `rewrite_increments_generation`, `partial_line_does_not_advance_cursor`, `journal_commit_is_idempotent`, `quarantine_advances_atomically`, `unclassified_cannot_serialize_or_enter_sink`, `complete_receipt_required_for_observation`, `serialized_fields_scan_independently`, `scan_failure_commits_skeleton_not_content`, `exact_replay_rejects_digest_substitution`, `capture_preserves_multi_repo_worktree_generation_scope`, `empty_scope_is_not_current_project`, and `scope_candidates_never_replace_requested_scope`.
- [ ] Add the public signatures above and exhaustive enums with serde tags fixed to `snake_case`.
- [ ] Implement canonical identity bytes, compare-and-set source state, record framing, Plan 18's parse-before-scan engine/policy/receipts/bounded detector registry, replay manifests, and runner retry semantics. Make sanitized observation the only journal/spool input; retire message-metadata opt-out semantics.
- [ ] Add architecture lint that rejects imports matching `tracedecay::sessions`, `tracedecay::hooks`, `tracedecay::automation`, `mcp`, or `dashboard` from the crate.
- [ ] Run `cargo test -p tracedecay-capture --test contract_suite`; expected: exit 0 and all fourteen named contracts pass.
- [ ] Run `cargo clippy -p tracedecay-capture --all-targets --all-features -- -D warnings`; expected: exit 0 with no warnings.
- [ ] Commit `feat(capture): add deterministic observation runner`.

### PR 7B: Durable hook spool and concurrent-agent capture

**Files:** create `src/spool/{mod,client,frame,recovery}.rs`, `src/hook.rs`, `tests/hook_spool_suite.rs`, `benches/capture.rs`.

- [ ] Write failing tests for 128 concurrent producer lanes, same-lane monotonic sequence, duplicate drain, sequence gap/fill, out-of-order occurred time, crash before/after `fdatasync`, crash before/after journal ack, corrupt tail truncation, `0600` O_EXCL fallback/lock/JSONL writes, symlink rejection, overflow marker reservation, and two competing drainers.
- [ ] Implement framed hash-chained segments, pending-lane rotation, contiguous acks, lease/CAS drain, recovery scan, soft/hard backpressure, and diagnostics stated above.
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
- Projected visibility is measured end-to-end by the projector plan and must be at or below two seconds p95 before cutover.
- Spool recovery of 1 million frames completes without loading all payloads into memory; benchmark records peak RSS.

### Privacy

- Committed secret corpus yields zero secret-bearing FTS/vector/fact/fixture/export/log hits.
- Files, spool segments, quarantine blobs, and manifests are private; hash/permission doctor tests pass.
- Reasoning capture is opt-in, provider-exposed only, shorter-retained, and excluded from search/export by default.
- Locked privacy domains expose metadata/coverage only; capture never falls back to plaintext.

### Observability

- Metrics expose discovery, bytes/records scanned, source generation/cursor, ingest rate/lag, duplicates, rewrites, gaps/fills, late records, spool bytes/oldest age, ack lag, backpressure, errors/quarantine, parser/schema coverage, redactions, and cutover epoch.
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
