# TraceDecay V2 Stable Anchors and Provenance

## Status / Role

Status: active product contract.

Role: PR7 establishes stable evidence anchors for captured observations. Later query,
search, API, and UI slices preserve and resolve those anchors. This plan does not
create a research-management system.

## Outcome

Any authorized result can lead back to the exact retained observation or entity that
supports it. The reference survives ranking changes, project moves, worktree removal,
and index rebuilds, while deletion and retention remain explicit.

## Owns

- `RetrievalAnchorId` identity and resolution semantics as the canonical lossless
  reference for sanitized retained evidence.
- Target kinds including, at minimum: session and observation evidence; GitHub
  review-thread, comment, and reply evidence; CI log and artifact excerpts;
  diagnostics; and related retained source evidence joined to those products.
- Provenance relations such as `captured_from`, `produced`, `observed`, `executed_in`,
  `discussed`, `copied_from`, and `derived_from`.
- Evidence time, source generation, projection watermark, coverage, and drift state.
- Immutable Git evidence coordinates: canonical repository identity; commit,
  tree, and blob object identity; parent/side role; path identity; and retained
  index or worktree-capture watermark when no immutable Git object exists.
- PR/comment coordinates bound through
  [Plan 36](36-git-aware-change-context-and-index-transactions.md)
  `PullRequestSnapshot`, `ReviewThreadAnchor`, and `CommentAnchor` identity.
- Safe tombstones for expired, redacted, deleted, unavailable, or ambiguous targets.
- Rules for distinguishing direct authorship from copied coordination text.
- Immutable derived evidence-span identity over exact source occurrences, including
  source-order evidence, projector identity, temporal horizon, sanitization receipts,
  replay, drill-down, and copy/summary lineage.
- Payload-free retriever-contribution anchors that explain which exact retained
  sources contributed to an assembled result without making rank, score, query text,
  summaries, or embeddings source authority.

## Does not own

- Research manifests, research ledgers, private corpus registries, or subagent rosters.
- Plan validation, progress tracking, compatibility inventories, or implementation
  workflow enforcement.
- Physical storage schema, ranking, scope resolution, authorization policy, transport
  routes, or presentation.
- Embedded transcript payloads or alternate paths around current authorization.
- Transport `rh_` response handles, MCP task IDs, workflow IDs, or collection
  cursors. Those are transport or paging artifacts, not durable evidence identity.
- GitHub API ingress, comment writes, or CI execution authority.
- Candidate generation, ranking, diversification, temporal answer selection, summary
  payload publication, or context rendering. Those remain
  [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md) responsibilities.
- Task/work identity or graph state. Those remain
  [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md)
  responsibilities.
- Host capability definitions, catalog generation, adapter packaging, or provider
  event decoding. Those remain
  [Plan 27](27-cross-host-agent-plugin-bundles.md) responsibilities.

## Required behavior

1. An anchor is a stable opaque `RetrievalAnchorId`, not a search query,
   transport `rh_` response handle, collection cursor, rank, file path, branch
   name, timestamp, or content hash. IDs never embed payload bytes.
2. Owning ingress paths create anchors in the same authoritative transaction as the
   retained sanitized evidence and its source identity for that target kind. PR7
   covers observation anchors; PR13 read-only GitHub and CI ingress covers those
   evidence classes. Retry returns the existing anchor.
3. Each anchor records target kind, canonical owner, native aliases when available,
   occurred and ingested time, source generation, projection watermark, and evidence
   class. It does not copy the target payload.
4. Resolution rechecks current authorization and privacy policy on every use. It never
   grants access because a caller possesses an ID and never leaks an unauthorized
   target's existence.
5. Resolution reports `current`, `drifted`, `redacted`, `expired`, `deleted`,
   `unavailable`, or `ambiguous` with coverage. It never silently switches owner,
   provider, project, session variant, or source generation.
6. Project moves, aliases, and worktree removal update routing, not anchor identity.
   A retained anchor remains globally routable within its authorized profile.
7. Derived summaries, search documents, graph nodes, and reports retain source-anchor
   lineage. A derived object cannot become its own unsupported evidence source.
8. Copied parent prompts, provider protocol records, and repeated coordination messages
   may be related evidence but cannot establish direct human authorship or child-task
   ownership without provider linkage or an explicit attribution assertion.
9. Retention removes payload access according to policy while preserving the minimum
   safe tombstone needed to explain the target state and prevent ID reuse.
10. Later query slices return anchors for exact results, omissions, and explanations;
    transport and UI layers pass them through without defining another reference type.
11. A Git anchor never treats a branch, tag, symbolic ref, checkout path, or current
    `HEAD` as immutable evidence. PR7 resolves routing inputs to exact retained Git
    objects or a receipt-bound index/worktree capture in the authoritative anchor
    transaction; ref movement cannot change what an existing anchor means.
12. Commit, tree, and blob anchors preserve native object identity and repository
    ownership. Patch hunks use the PR9 `HunkRef`, which references anchored sides (or
    captured mutable-state watermarks) plus native Git diff options and coordinates;
    it does not create a second content or provenance identity.
13. GitHub thread, comment, and reply anchors bind sanitized retained provider
    evidence to Plan 36 `ReviewThreadAnchor`/`CommentAnchor` and
    `PullRequestSnapshot` identity. Remapped review coordinates are never reported
    `current` unless exact content and anchor coordinates match.
14. CI log and artifact-excerpt anchors retain sanitized bounded excerpts with source
    run, job, step, artifact, and time provenance. They reference CI authority; they
    do not claim pass/fail outcome authority.
15. Diagnostic anchors bind to canonical provider/diagnostic identity from
    [Plan 09](09-application-crate.md) and
    [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) without inventing a
    second finding model.
16. Git provenance, capture/projection watermarks, and later code-index generation
    watermarks remain separate typed evidence. Resolution reports each and any drift;
    path/line similarity cannot silently upgrade mismatched evidence.

## Immutable evidence-span contract

PR7 adds one payload-free identity model for consecutive message, tool-invocation,
tool-result, and code-chunk occurrences. It does not reuse observation-level anchors,
`SessionSummaryRecordV1::source_anchors`, or sorted `RetrievalAnchorRecordV2`
lineage as an ordering model: those collections cannot distinguish multiple projected
outputs from one observation or preserve assembly order.

The exact domain types are:

- `ProfileId`, `SourceOccurrenceIdV1`, `CanonicalSourceOccurrenceSetIdV1`,
  `EvidenceSpanIdV1`, `RetrieverContributionIdV1`,
  `RetrievalAnchorDispositionIdV1`, `EvidenceAssemblyIdempotencyKeyV1`, and
  `PrivacyBoundRequestDigestV1` in
  `crates/tracedecay-domain/src/research/id.rs`. The existing application
  `ProfileId` moves to this domain newtype and `src/application/context.rs` uses it;
  there is one profile identity type.
- `AnchorOwnerBindingV1 { profile_id, project_id, owner_shard,
  privacy_domain_id }` in `crates/tracedecay-domain/src/research/anchor.rs`.
  `project_id` is absent only for explicitly profile-owned evidence; a path, CWD,
  store filename, project label, host profile, PID, branch, or ref cannot fill it.
- `SourceOccurrenceKindV1::{Message, ToolInvocation, ToolResult, CodeChunk}`,
  `SourceOccurrenceRelationV1`, `SourceTimelineKeyV1`,
  `SanitizedObservationByteRangeV1`, `SourceOccurrenceCoordinateV1`,
  `ProjectorSnapshotV1`, `SourceOccurrenceRecordV1`,
  `SourceOccurrenceRecordV1Parts`, `SourceOccurrenceSanitizationV1`,
  `EvidenceSpanProjectionReceiptV1`, `VerifiedSourceOrderingProofV1`,
  `CanonicalSourceOccurrenceSetV1`, `EvidenceSpanRunV1`,
  `SourceCapabilityCatalogBindingV1`, `EvidenceSpanCatalogBindingV1`,
  `EvidenceSpanHorizonV1`, `EvidenceSpanRecordV1Parts`,
  `EvidenceSpanIdentityMaterialV1`, `EvidenceSpanRecordV1`, and
  `EvidenceSpanError` in
  `crates/tracedecay-domain/src/research/evidence_span.rs`.
- `RetrieverIdentityV1`, `RetrieverWatermarkBindingV1`,
  `RetrieverContributionRecordV1Parts`, and `RetrieverContributionRecordV1` in
  `crates/tracedecay-domain/src/research/retriever_contribution.rs`.
- `AnchorLineageRefV3` in `crates/tracedecay-domain/src/research/anchor.rs`.
  It adds explicit `AnchorOwnerBindingV1` for child and source while preserving
  byte-for-byte V2 decoding.
- `RetrievalAnchorDispositionRecordV1`, `RetrievalAnchorTombstoneV1`, and
  `EvidenceAssemblyResolutionStateV1` in
  `crates/tracedecay-domain/src/research/resolution.rs`.

`SourceTimelineKeyV1` contains the provider/source identity,
`ObservationScopeV1`, `ObservationSourceGenerationV1`, and
`ObservationOrderingDomainV1`. `SourceOccurrenceCoordinateV1` is a closed enum:

- `ObservationProjection { canonical_observation_id,
  source_range, projection_output_ordinal, sanitized_byte_range }` for message
  and tool occurrences;
- `ImmutableBlobSlice { repository_id, blob_id, byte_start, byte_end }`; or
- `CapturedWorktreeSlice { repository_id, repository_capture_id,
  path_locator_digest, byte_start, byte_end }`.

Code coordinates are half-open byte ranges over the anchored blob or receipt-bound
capture. `SanitizedObservationByteRangeV1` is a half-open byte range over the
versioned canonical sanitized-observation encoding identified by the exact source
anchor and capture receipt; it is not a Unicode character, token, provider field, or
raw-file offset. Line numbers, current paths, ambient `HEAD`, snippets, and symbol
names are display metadata only.

`SourceOccurrenceRelationV1` contains
`ToolResultFor { invocation_occurrence_id }` and
`DerivedFromOccurrence { source_occurrence_id }`. Every `ToolResult` has exactly one
`ToolResultFor` relation and that target must be a `ToolInvocation` in the same owner
and exact `SourceTimelineKeyV1`. This pairing, not proximity, disambiguates
concurrent or interleaved calls.

For identity, `ObservationSourceRangeV1` serializes canonically as the
`ObservationOrderingDomainV1` tag followed by unsigned 64-bit big-endian
`start` and `end` values for a half-open interval. `FileBytes` values are native
source bytes; `SqliteRowId`, `SnapshotOrder`, and `DaemonSequence` values are
ordinal intervals in that named domain and are never interpreted as bytes. The
separate `SanitizedObservationByteRangeV1` always addresses the versioned canonical
sanitized-observation byte encoding. UTF-8, CRLF, or sanitizer changes cannot
reinterpret either coordinate.

`SourceOccurrenceRecordV1::new(parts: SourceOccurrenceRecordV1Parts)` is pure and
derives `SourceOccurrenceIdV1` from a versioned domain separator, owner binding,
the complete canonical `SourceTimelineKeyV1` bytes, exact source anchor and
coordinate, occurrence kind and relations, and projector `ComponentVersion`.
Changing provider/source identity, scope, source generation, ordering domain,
projector version, coordinate, relation, or exact source rekeys the occurrence.
Projection generation and `VectorWatermark` are excluded so an exact rebuild by the
same projector version reproduces the occurrence ID. A projector-version or
exact-source change creates a new occurrence and an `AnchorLineageRefV3::DerivedFrom`
edge; it never retargets an old ID.

`SourceOccurrenceSanitizationV1::new(capture, projection)` has distinct required
`capture: SanitizationReceiptRefV1` and
`projection: SanitizationReceiptRefV1` fields; a set or one receipt cannot satisfy
both roles. `EvidenceSpanProjectionReceiptV1::new(span_id, projector_snapshot,
member_receipts)` records projection generation/watermark and one role-complete
receipt binding per member as an append-only rebuild receipt. These receipts and
rebuild watermarks are immutable provenance but are outside
`EvidenceSpanIdentityMaterialV1`; the same span can therefore retain multiple exact
rebuild receipts without mutating its identity row. Changed retained sanitized bytes
require a new source anchor and occurrence ID.

`CanonicalSourceOccurrenceSetV1::new(owner, members:
Vec<SourceOccurrenceRecordV1>)` performs only I/O-free structural validation and
rejects an empty set, duplicates, owner/privacy mismatches, and invalid records. It
sorts by `SourceOccurrenceIdV1` only to derive
`CanonicalSourceOccurrenceSetIdV1`; that set identity proves membership, not order.
It never hashes payload, summary text, embeddings, scores, timestamps, or query text.
The constructor assigns `canonical_ordinal = 0..n-1` in this same
`SourceOccurrenceIdV1` order, and both `record_digest` and persistent member rows use
that exact sequence. Two input permutations therefore produce one byte-identical set
record.
`PublishEvidenceAssembly::execute` resolves every source anchor and verifies both
receipt roles through `SanitizationReceiptResolverV1` before the store accepts the
set; constructors do not perform store or catalog I/O.

`EvidenceSpanRunV1::new(timeline, proof: VerifiedSourceOrderingProofV1, members)`
is pure, preserves caller order, and requires one timeline key and strictly
source-ordered members. `SourceCapabilityEvidenceVerifier::verify` in
`src/application/evidence/ports.rs` is the only factory for
`VerifiedSourceOrderingProofV1`; it checks the Plan 08 catalog digest, Plan 27
connector/root/capability binding, integration-manifest digest, configuration and
authorization-scope digests, projector revision, source watermark, and adjacency
claim before construction. Numeric-looking IDs, timestamps, adjacent byte ranges,
or missing intervening retained rows do not prove consecutiveness. A known gap starts
another run. Verification fails with typed `EvidenceSpanError::{CatalogMismatch,
IntegrationManifestMismatch, StaleOrderingProof, IncomparableSourceOrder,
UnverifiedConsecutiveness}`.

`EvidenceSpanRecordV1::new(EvidenceSpanRecordV1Parts)` requires an ordered,
non-empty run list. Flattened run membership must equal its canonical occurrence set
exactly, with no omission, duplicate, or substitution. Runs from different timelines
have explicit `assembly_ordinal` order only; that order asserts no global chronology,
valid-time order, happened-before relation, or causality. Reversing cross-source runs
changes `EvidenceSpanIdV1` but does not create chronological evidence.

`EvidenceSpanRecordV1::new(parts: EvidenceSpanRecordV1Parts)` first creates
`EvidenceSpanIdentityMaterialV1`, then derives `EvidenceSpanIdV1` only from the
canonical serialization of that projection. The identity projection contains owner,
canonical occurrence-set ID, ordered run/member IDs, projector component version,
the knowledge-through/valid-through/unknown-valid-time fields from
`EvidenceSpanHorizonV1`, and `EvidenceSpanCatalogBindingV1`. The catalog binding is
`IntrinsicCanonicalOrdering` when the domain itself proves order, or
`SourceCapability(SourceCapabilityCatalogBindingV1)` when decoding, normalization,
ordering, or adjacency depends on a source capability.

`SourceCapabilityCatalogBindingV1` contains the Plan 27
`PlannerSourceDescriptorV1` connector/root identity and projector revision, the
selected Plan 08 `CapabilityId` and `CatalogDigest`, and the Plan 27
`HostIntegrationManifestV1` digest, configuration digest, authorization-scope
digest, and source watermark. Plan 08 remains sole callable-capability/catalog
authority; Plan 27 owns the host manifest, connector binding, descriptor projection,
and projector revision. This binding is not research
`CatalogSnapshotRefV1`. Projection rebuild generation, validation watermarks,
receipt IDs, created/ingested time, rank, score, query, cursor, summary, embedding,
and payload hashes are not span identity.

`EvidenceSpanHorizonV1` records exact knowledge-through and valid-through bounds,
and whether any member has unknown valid time. Its constructor rejects bounds that
do not cover every member. Frozen source/projection watermarks live only in
`EvidenceSpanProjectionReceiptV1` and `RetrieverWatermarkBindingV1`. Plan 23 owns
temporal-mode selection; the horizon preserves the selected boundary without
redefining `TemporalModeV1`.

`RetrievalAnchorTargetV3` adds `ExactSourceOccurrence(SourceOccurrenceIdV1)`,
`ExactEvidenceSpan(EvidenceSpanIdV1)`, and
`RetrieverContribution(RetrieverContributionIdV1)`. Because V2 records are already
persisted, `RetrievalAnchorRecordV3` extends rather than silently changing V2 wire
semantics. `derive_exact_source_occurrence_anchor_id`,
`derive_exact_evidence_span_anchor_id`, and
`derive_retriever_contribution_anchor_id` call the existing canonical digest
machinery. Public lookup still uses only `RetrievalAnchorId`; the new IDs identify
immutable targets and do not create a parallel public reference family.

## Retriever-contribution evidence

Plan 23 emits a `RetrieverContributionRecordV1` after it freezes scope, temporal
mode, source/projection/index/summary watermarks, and selected exact sources. The
record contains:

- derived contribution ID and its `RetrievalAnchorId`;
- `AnchorOwnerBindingV1`;
- `RetrieverIdentityV1 { capability_id, component_version }`;
- `SourceCapabilityCatalogBindingV1`;
- `PrivacyBoundRequestDigestV1`, `ScopeResolutionId`, and exact
  `TemporalModeV1`;
- `RetrieverWatermarkBindingV1` with separately typed source, projection, index,
  and summary watermarks;
- canonical occurrence-set ID, evidence-span ID and anchor, exact source anchors,
  `CoverageReportV1`, canonical record digest, and creation time.

`PrivacyBoundRequestDigestV1` is a keyed digest with privacy-domain ID and key epoch.
Its canonical preimage is exactly `{ UseCaseId, ScopeResolutionId, TemporalModeV1,
EvidenceSpanHorizonV1, sorted requested CapabilityId values }`; it excludes query
text, prompt text, paths, symbols, provider payload, snippets, embeddings, and model
prose. Equal request envelopes in different privacy domains or key epochs are
unlinkable. `RetrieverContributionRecordV1::new` requires the digest privacy-domain
ID to equal `AnchorOwnerBindingV1::privacy_domain_id`; a key-epoch mismatch or
cross-domain reuse is `EvidenceSpanError::RequestPrivacyBindingMismatch`.

`RetrieverContributionRecordV1::new(parts:
RetrieverContributionRecordV1Parts)` is pure and derives identity from every
immutable binding above except the anchor ID, record digest, and creation time. It
does not read storage, return an existing row, or roll back. A changed retriever
version, privacy-bound request digest, scope, temporal mode, catalog/manifest/config
binding, frozen watermark, occurrence set, or assembly order creates a new
contribution.

`EvidenceAssemblyWriteV1::new(idempotency_key, occurrence_set, span,
projection_receipt, contribution, anchors, lineage)` binds the transaction.
`EvidenceAssemblyIdempotencyKeyV1::derive(owner, key_epoch, raw_request_key)` uses a
versioned privacy-domain key to derive a digest over the canonical owner digest,
privacy-domain ID, key epoch, and caller key; raw key bytes are never persisted.
`EvidenceAssemblyStore::publish_or_replay` returns the existing receipt only when the
same owner/privacy/key-epoch-bound `EvidenceAssemblyIdempotencyKeyV1` has the same
canonical assembly digest. The same scoped key with different material is
`EvidenceAssemblyStoreError::ReplayConflict` and rolls back every row. The same raw
caller key in another owner/privacy domain neither collides nor reveals occupancy.

A contribution is explanation evidence, not source evidence. Rank, score, fusion
weight, candidate position, query text, embedding/vector identity, summary text,
model prose, and transport state cannot identify or retarget it. Ranking or model
changes that leave every immutable binding equal replay the same contribution;
changes to selected sources or order create a new span/contribution. Drill-down is
lossless and typed:

```text
retriever-contribution RetrievalAnchorId
  -> EvidenceSpanIdV1 and span RetrievalAnchorId
  -> CanonicalSourceOccurrenceSetIdV1
  -> ordered runs and exact source-occurrence RetrievalAnchorIds
  -> current-authorized owning-store payloads
```

Every hop rechecks current authorization, privacy, retention, disposition, catalog
binding, and drift. The records and tables contain no hydrated text or provider
payload.

## Authorization, lineage, and deletion

Every create, resolve, hydrate, expand, replay, and cursor-continuation operation
binds to `AnchorOwnerBindingV1` and accepts the current Plan 09 `RequestContext`.
Creation-time `ResolutionAuthorizationV1` is provenance only. Resolution authorizes
the exact target immediately before any payload or existence disclosure; possessing,
copying, or guessing an opaque ID grants nothing. Denied and unknown targets are
externally indistinguishable.

Native aliases use a privacy-domain-keyed locator digest with an explicit key epoch.
An unkeyed content/path hash or raw provider locator is not a
`PrivacyDomainBoundLocatorDigest`. Equal locator material in different privacy
domains or key epochs produces unlinkable aliases.

All new durable lineage uses `AnchorLineageRefV3` with child/source anchor ID and
explicit canonical owner/privacy binding. V2 rows remain decodable and migrate only
after exact owner/privacy resolution; unverifiable V2 lineage stays typed
`UnverifiableLegacy` and cannot serve. Publication atomically writes forward and
reverse lineage. A provider-native copied message remains a distinct source
occurrence and uses `LogicalCopyRecordV1`/`CopiedFrom`; it proves only the copy's
authorship and cannot impersonate its source. A summary uses
`SessionSummaryRecordV1` plus exact owner-bound source-span/occurrence anchors.
Summary text and embedding identity are never canonical members of a
source-occurrence set. If a summary accelerates a contribution, the contribution
still retains the exact underlying source anchors.

Redaction, expiry, deletion, quarantine, correction, and legal-hold changes append
`RetrievalAnchorDispositionRecordV1` rows outside immutable anchor identity.
Resolution applies the newest authoritative disposition before reading anchors,
aliases, lineage, payloads, summaries, snippets, FTS/index rows, caches, exports,
backups, or replicas. Immutable occurrence, span, contribution, anchor, and lineage
rows are never updated or deleted: an appended disposition makes them unresolvable.
The corresponding payload, cache, summary body, copied payload, FTS row, and
derivative index row is purged or suppressed before any can serve; no derivative
becomes fallback authority.

`RetrievalAnchorTombstoneV1` has a strict safe-field whitelist: opaque anchor ID,
terminal state, non-sensitive policy/reason class, effective time, and the minimum
owner-shard routing proof required to prevent ID reuse. It contains no payload,
snippet, alias, native locator, target coordinate, source ID, query, rank, path,
timestamp from the source, or hidden-owner coverage. Unauthorized callers receive no
tombstone or existence distinction. Restore, consolidation, replay, and migration
apply current dispositions before importing or rebuilding derivatives, so stale
copies cannot resurrect payload access.

## Implementation allocation and migration gates

PR7 implementation is allocated exactly as follows:

- `crates/tracedecay-domain/src/research/evidence_span.rs` owns occurrence-set,
  run, span, horizon, catalog-binding, identity derivation, and validation types.
- `crates/tracedecay-domain/src/research/retriever_contribution.rs` owns immutable
  contribution records and structural validation.
- `crates/tracedecay-domain/src/research/{id,anchor,resolution,subjects,mod}.rs`
  owns the new IDs, V3 targets/records, owner binding, dispositions/tombstones,
  entity kinds, and exports.
- `crates/tracedecay-store/src/evidence/{mod,write,read,migration}.rs` owns
  `EvidenceAssemblyWriteV1::new`, `EvidenceAssemblyReceiptV1`,
  `EvidenceAssemblyStoreError`, and the `EvidenceAssemblyStore`
  `publish_or_replay`/payload-free resolve port. It never accepts payload fields.
- `src/application/evidence/{mod,ports,publication,resolution}.rs` owns the Plan 09
  authorized commands and the `SourceCapabilityEvidenceVerifier` port.
  `PublishEvidenceAssembly::execute` resolves anchors, verifies both sanitization
  receipt roles and source-order proof, then atomically writes the source-occurrence
  set, ordered span, projection receipt, contribution, anchors, V3 reverse lineage,
  and replay receipt; `ResolveEvidenceAssembly::execute` reauthorizes every hop.
- `src/global_db/evidence_assembly/{mod,schema,write,read,migration}.rs` owns the
  physical adapter. `src/global_db/schema_stages.rs`,
  `src/global_db/schema_contract/definitions.rs`,
  `src/global_db/schema_contract/invariants/rows.rs`, and
  `src/global_db/schema_contract/invariants/triggers.rs` register and audit it.
- `src/migrate/consolidate/sqlite/evidence_assembly.rs` merges dispositions first
  and then eligible immutable records; it never copies snippets, summary text, FTS
  text, or hydrated payload.

The additive migration is named `20260718_evidence_assembly_v1`. Its
`MIGRATION_NAME` is `"evidence-assembly"` and
`EVIDENCE_ASSEMBLY_SCHEMA_VERSION` is `1`. It creates
`evidence_assembly_schema_migrations(name TEXT PRIMARY KEY, version INTEGER NOT
NULL, applied_at INTEGER NOT NULL)` before:
`source_occurrences`, `evidence_span_projection_receipts`,
`evidence_span_projection_receipt_members`,
`canonical_source_occurrence_sets`,
`canonical_source_occurrence_set_members`, `evidence_spans`,
`evidence_span_runs`, `evidence_span_members`, `retriever_contributions`,
`retriever_contribution_sources`, `retrieval_anchor_dispositions`,
`retrieval_anchor_lineage_reverse`, and `evidence_assembly_replay_receipts`.

The schema manifest in `src/global_db/evidence_assembly/schema.rs` declares these
exact columns below. Every listed column is `NOT NULL`; digest, ID, enum, and JSON
columns are `TEXT`; ordinal, epoch, time, and version columns are `INTEGER`; and
every foreign key uses `ON UPDATE RESTRICT ON DELETE RESTRICT`.

- `source_occurrences(occurrence_id TEXT PRIMARY KEY, anchor_id TEXT UNIQUE,
  owner_json TEXT, owner_digest TEXT, privacy_domain_id TEXT, timeline_json TEXT,
  timeline_digest TEXT, source_anchor_id TEXT, kind TEXT, coordinate_json TEXT,
  relations_json TEXT, projector_version TEXT, record_digest TEXT)`;
- `evidence_span_projection_receipts(receipt_id TEXT PRIMARY KEY, span_id TEXT,
  projection_generation TEXT, projection_watermark_json TEXT, record_digest TEXT,
  UNIQUE(receipt_id, span_id))` and
  `evidence_span_projection_receipt_members(receipt_id TEXT, span_id TEXT,
  member_ordinal INTEGER, occurrence_id TEXT, capture_receipt_id TEXT,
  projection_receipt_id TEXT, PRIMARY KEY(receipt_id, member_ordinal),
  UNIQUE(receipt_id, occurrence_id))`;
- `canonical_source_occurrence_sets(set_id TEXT PRIMARY KEY, owner_json TEXT,
  owner_digest TEXT, privacy_domain_id TEXT, record_digest TEXT)` and
  `canonical_source_occurrence_set_members(set_id TEXT,
  canonical_ordinal INTEGER, occurrence_id TEXT,
  PRIMARY KEY(set_id, canonical_ordinal), UNIQUE(set_id, occurrence_id))`;
- `evidence_spans(span_id TEXT PRIMARY KEY, anchor_id TEXT UNIQUE, set_id TEXT,
  owner_json TEXT, owner_digest TEXT, privacy_domain_id TEXT,
  projector_version TEXT, horizon_json TEXT, catalog_binding_json TEXT,
  record_digest TEXT)`, `evidence_span_runs(span_id TEXT, run_ordinal INTEGER,
  timeline_json TEXT, timeline_digest TEXT, ordering_proof_digest TEXT,
  PRIMARY KEY(span_id, run_ordinal))`, and
  `evidence_span_members(span_id TEXT, run_ordinal INTEGER,
  member_ordinal INTEGER, occurrence_id TEXT,
  PRIMARY KEY(span_id, run_ordinal, member_ordinal),
  UNIQUE(span_id, occurrence_id))`;
- `retriever_contributions(contribution_id TEXT PRIMARY KEY,
  anchor_id TEXT UNIQUE, owner_json TEXT, owner_digest TEXT,
  privacy_domain_id TEXT, connector_id TEXT, root_id TEXT, capability_id TEXT,
  component_version TEXT, catalog_digest TEXT,
  integration_manifest_digest TEXT, configuration_digest TEXT,
  authorization_scope_digest TEXT, projector_revision TEXT,
  source_watermark_json TEXT, request_digest TEXT, request_key_epoch INTEGER,
  scope_resolution_id TEXT, temporal_mode TEXT, span_id TEXT, set_id TEXT,
  watermarks_json TEXT, coverage_json TEXT, record_digest TEXT,
  created_at INTEGER)` and
  `retriever_contribution_sources(contribution_id TEXT,
  source_ordinal INTEGER, source_anchor_id TEXT,
  PRIMARY KEY(contribution_id, source_ordinal),
  UNIQUE(contribution_id, source_anchor_id))`;
- `retrieval_anchor_dispositions(disposition_id TEXT PRIMARY KEY,
  anchor_id TEXT, disposition TEXT, reason_class TEXT, effective_at INTEGER,
  authority_epoch INTEGER, receipt_digest TEXT,
  UNIQUE(anchor_id, authority_epoch))`;
- `retrieval_anchor_lineage_reverse(child_anchor_id TEXT,
  source_anchor_id TEXT, relation TEXT, child_owner_digest TEXT,
  source_owner_digest TEXT, privacy_binding_digest TEXT,
  PRIMARY KEY(child_anchor_id, source_anchor_id, relation))`; and
- `evidence_assembly_replay_receipts(owner_digest TEXT, privacy_domain_id TEXT,
  key_epoch INTEGER, idempotency_key_digest TEXT, assembly_digest TEXT,
  contribution_id TEXT, committed_at INTEGER,
  PRIMARY KEY(owner_digest, privacy_domain_id, key_epoch,
  idempotency_key_digest))`.

The exact foreign keys are:
`source_occurrences.anchor_id -> retrieval_anchors.anchor_id`;
`source_occurrences.source_anchor_id -> retrieval_anchors.anchor_id`;
`evidence_span_projection_receipts.span_id -> evidence_spans.span_id`;
`evidence_span_projection_receipt_members.(receipt_id, span_id) ->
evidence_span_projection_receipts.(receipt_id, span_id)`;
`evidence_span_projection_receipt_members.(span_id, occurrence_id) ->
evidence_span_members.(span_id, occurrence_id)`;
`canonical_source_occurrence_set_members.set_id ->
canonical_source_occurrence_sets.set_id`;
`canonical_source_occurrence_set_members.occurrence_id ->
source_occurrences.occurrence_id`;
`evidence_spans.anchor_id -> retrieval_anchors.anchor_id`;
`evidence_spans.set_id -> canonical_source_occurrence_sets.set_id`;
`evidence_span_runs.span_id -> evidence_spans.span_id`;
`evidence_span_members.(span_id, run_ordinal) ->
evidence_span_runs.(span_id, run_ordinal)`;
`evidence_span_members.occurrence_id -> source_occurrences.occurrence_id`;
`retriever_contributions.anchor_id -> retrieval_anchors.anchor_id`;
`retriever_contributions.span_id -> evidence_spans.span_id`;
`retriever_contributions.set_id -> canonical_source_occurrence_sets.set_id`;
`retriever_contribution_sources.contribution_id ->
retriever_contributions.contribution_id`;
`retriever_contribution_sources.source_anchor_id ->
retrieval_anchors.anchor_id`;
`retrieval_anchor_dispositions.anchor_id -> retrieval_anchors.anchor_id`;
`retrieval_anchor_lineage_reverse.(child_anchor_id, source_anchor_id) ->
retrieval_anchors.anchor_id` as two separate foreign keys; and
`evidence_assembly_replay_receipts.contribution_id ->
retriever_contributions.contribution_id`.

Required indexes are
`idx_source_occurrences_source_anchor(source_anchor_id)`,
`idx_span_projection_receipts_span(span_id, projection_generation)`,
`idx_span_members_occurrence(occurrence_id)`,
`idx_contribution_sources_anchor(source_anchor_id)`,
`idx_lineage_reverse_source(source_anchor_id)`, and
`idx_anchor_dispositions_current(anchor_id, authority_epoch DESC)`.
The exact immutable deny triggers are
`source_occurrences_immutable_update`,
`source_occurrences_immutable_delete`,
`evidence_span_projection_receipts_immutable_update`,
`evidence_span_projection_receipts_immutable_delete`,
`evidence_span_projection_receipt_members_immutable_update`,
`evidence_span_projection_receipt_members_immutable_delete`,
`canonical_source_occurrence_sets_immutable_update`,
`canonical_source_occurrence_sets_immutable_delete`,
`canonical_source_occurrence_set_members_immutable_update`,
`canonical_source_occurrence_set_members_immutable_delete`,
`evidence_spans_immutable_update`,
`evidence_spans_immutable_delete`,
`evidence_span_runs_immutable_update`,
`evidence_span_runs_immutable_delete`,
`evidence_span_members_immutable_update`,
`evidence_span_members_immutable_delete`,
`retriever_contributions_immutable_update`,
`retriever_contributions_immutable_delete`,
`retriever_contribution_sources_immutable_update`,
`retriever_contribution_sources_immutable_delete`,
`retrieval_anchor_lineage_reverse_immutable_update`,
`retrieval_anchor_lineage_reverse_immutable_delete`,
`evidence_assembly_replay_receipts_immutable_update`, and
`evidence_assembly_replay_receipts_immutable_delete`;
`retrieval_anchor_dispositions` has
`retrieval_anchor_dispositions_append_only_update` and
`retrieval_anchor_dispositions_append_only_delete`. No table has
payload-bearing text, snippet, query, path, argument, result, summary, embedding,
or hydrated-payload columns.

Migration enables writes only after all of these gates pass:

1. reject a database newer than the supported evidence-assembly schema;
2. verify exact table, column, foreign-key, index, and trigger shapes;
3. backfill only occurrences with an exact source anchor, owner/privacy binding,
   source generation/order, projector version, coordinate, and verified receipts;
4. backfill a multi-member run only when adjacency is provable; a verifiable single
   occurrence may become a singleton run, while other legacy rows remain
   `UnverifiableLegacy` and never receive a synthetic content-hash span;
5. verify every flattened span exactly equals its canonical occurrence set; every
   Plan 08 `CatalogDigest` matches the recorded capability; every Plan 27 integration
   manifest/configuration/authorization/projector revision matches the ordering
   proof; every projection receipt's member set exactly equals its referenced span
   member set; and every canonical digest replays identically;
6. run dispositions-first restore/consolidation and prove no ineligible derivative
   payload or index row survives; and
7. activate reads and writes only after atomicity, authorization, replay-conflict,
   tombstone-whitelist, and payload-free schema audits pass.

Any failure leaves the old read path authoritative and rolls back every new
set/span/contribution/lineage/receipt row. Rollback never re-enables a deleted or
redacted payload.

## Cross-plan ownership

- Plan 13 owns `RetrievalAnchorId`, V3 anchor targets and lineage,
  occurrence-set/span/
  contribution identity, owner-bound lineage, replay semantics, resolution states,
  dispositions, and minimum-safe tombstones. Plan 02 owns generic persistence and
  migration policy/primitives; Plan 13's PR7 owns the evidence-assembly schema
  contract and adapter listed above. Plan 09 owns current authorization and
  transaction orchestration; Plan 18 owns sanitization and disposition policy.
- Plan 23 owns candidate generation, ranking, temporal selection, summary DAG
  payloads, and context assembly. It calls the Plan 13 constructors after freezing
  inputs and may render `CompactContextBundleV1`, but it cannot define another span
  identity, use a summary/embedding as canonical source, or copy external evidence
  into LCM.
- Plan 24 owns `TaskId`, work-item/version identity, task graph history, and
  task-domain projections. Tasks, handoffs, and experience records may cite Plan 13
  span/contribution anchors, but cannot copy payloads, reconstruct cross-source
  chronology, redefine evidence identity, or turn a contribution into task authority.
  Plan 24's `TaskEvidenceSpan` is therefore a task-domain binding view over
  `EvidenceSpanIdV1` and its `RetrievalAnchorId`; its work-item, coordinate, content
  digest, score, and representation fields cannot derive or re-key source evidence.
- Plan 08 owns callable source-capability definitions, stable `CapabilityId`, and
  `CatalogDigest`. Plan 27 owns host adapters, provider-native ordering evidence,
  `HostIntegrationManifestV1`, source connector/root bindings,
  `PlannerSourceDescriptorV1`, and projector revision. Plan 13 persists only
  `SourceCapabilityCatalogBindingV1` and validates both authorities; it does not
  define capabilities or host semantics. Host files and processes may transport
  anchor IDs but cannot mint authorization, resolve stores locally, persist anchor
  copies, sanitize independently, or infer owner identity from ambient host state.

## Lossless evidence boundary

Durable products resolve through `RetrievalAnchorId` plus owning-store retention
for sanitized payloads. [Plan 05](05-query-crate.md) opaque cursors page typed
collections only. Transport `rh_` response handles from
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) are 24-hour,
project-local output recovery for truncated MCP/CLI responses and are never
durable evidence identity, anchor targets, or storage keys. This plan does not
own response-handle implementation.

PR13 read-only GitHub thread/comment/reply and CI-failure ingress may create and
resolve these anchors without [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) as
a prerequisite. Plan 32 remains required only for PR17 write-side effects and
workflow automation outside this contract.

## Acceptance

- PR7 tests atomic observation-and-anchor creation, idempotent replay, rollback, native
  alias collisions, copied-prompt attribution, and unauthorized resolution.
- Rebuilding projections preserves anchor IDs and source lineage.
- `crates/tracedecay-domain/tests/evidence_span_contract.rs` proves deterministic
  source-occurrence/set/span IDs, mixed message/tool/code runs, exact ordering,
  cross-source assembly-only semantics, horizon validation, V3 wire compatibility,
  byte-identical canonical-set normalization across input permutations, catalog
  binding, same-timeline tool-result pairing, UTF-8/CRLF coordinate stability, and
  rejection of gaps, duplicates, owner/generation/timeline mismatch, bare offsets,
  content hashes, summaries, embeddings, rank, and query identity.
- `crates/tracedecay-domain/tests/retriever_contribution_contract.rs` proves exact
  replay, changed immutable-input rekeying, idempotency conflicts, source-set/span
  equality, owner/request privacy-domain and key-epoch equality, payload-free
  serialization, and independent Plan 08 catalog plus Plan 27 connector/root/
  manifest/configuration/authorization/projector/source-watermark tamper rejection.
- `crates/tracedecay-store/tests/evidence_assembly_contract.rs` proves one atomic
  set/span/contribution/anchor/lineage/receipt transaction, rollback on every
  conflict, immutable-table triggers, exact drill-down, authorization parity, and
  round-trip isolation for multiple rebuild receipts on two spans that share an
  occurrence; missing, extra, duplicate, and cross-span receipt members are rejected.
  The same raw idempotency key in two owner/privacy domains does not collide or reveal
  occupancy, while same-scope changed material returns `ReplayConflict`.
- `tests/session_suite/evidence_span_projection.rs` and
  `tests/session_suite/temporal_retriever_contributions.rs` prove same-version
  rebuild identity, new-projector `DerivedFrom` lineage, verified adjacency,
  singleton legacy handling, ranking-independent replay, and contribution -> span
  -> set -> exact source expansion.
- `tests/session_suite/anchor_tombstone_expiry.rs` and
  `tests/session_suite/lcm_summary_lineage_review.rs` prove strict tombstone fields,
  authorization revocation, possession-only denial, and transitive source deletion
  through span -> contribution -> nested summary -> FTS/context.
- `tests/storage_suite/evidence_assembly_migration.rs` proves
  `20260718_evidence_assembly_v1` exact columns, nullability, foreign keys, indexes,
  trigger names, shape/version refusal, exact-only backfill, dispositions-first
  restore/consolidation, repeatable migration receipts, rollback, and no payload
  resurrection.
- Moving refs, rewriting a branch, or removing a checkout does not retarget retained
  commit/tree/blob or captured-state anchors; unavailable objects return a safe typed
  state rather than resolving against ambient `HEAD`.
- Moving a project or deleting a worktree does not break a retained project/session
  anchor.
- Redaction, expiry, deletion, unavailable, and ambiguous targets return safe typed
  tombstones with no payload bytes.
- GitHub thread, comment, and reply anchors resolve through Plan 36 review identity,
  preserve remap lineage, and never report remapped coordinates as `current` without
  exact content-and-anchor match.
- CI log and artifact-excerpt anchors retain provenance and return typed
  drifted/redacted/expired/deleted/unavailable states without claiming CI authority.
- Diagnostic anchors resolve to canonical provider identity without a second finding
  model.
- Transport `rh_` handles and collection cursors cannot substitute for
  `RetrievalAnchorId` resolution in fixtures or product contracts.
- A search result can resolve to its exact source observation after ranking or index
  versions change, with drift and coverage reported.
- Reversing cross-source run assembly changes span identity without claiming
  chronology; source timestamps never create cross-source order.
- Summary text, copied text, model prose, embedding/vector identity, rank, score,
  mutable payload hashes, query/cursor/response handles, paths, and timestamps cannot
  substitute for a canonical source-occurrence set in domain, store, migration, or
  product fixtures.
- The same native locator in two profiles/projects/privacy domains or key epochs
  produces unlinkable aliases, and an unauthorized caller cannot distinguish a
  tombstone from an unknown anchor.
- Repository search finds no research-ledger, plan-parser, compatibility-inventory, or
  plan-execution requirement in this contract.
