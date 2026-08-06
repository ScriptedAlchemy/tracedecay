# TraceDecay V2 Stable Anchors and Provenance

## Status / Role

Status: active product contract.

Role: PR7 establishes stable evidence anchors for captured observations. Later query,
search, API, and UI slices preserve and resolve those anchors. This plan does not
create a research-management system.

Earlier type/file names, table and index inventories, cutover labels, fixture
paths, packet gates, and implementation allocations are historical evidence,
not prerequisites or artifacts that later work must recreate. Only actually
independently released public wire types may retain protocol compatibility.
Persisted records accept only their exact final shape; all other retention is
judged by the direct anchor identity, resolution,
authorization, lineage, deletion, Git, platform, and regression behavior
below.

The anchor/evidence V2/V3 shapes described below were introduced on this
integration branch and have no predecessor on `origin/master` or in a published
package/release. They use the V2 fresh-store cutover: every non-final database,
store, spool, file, or projection returns typed `ResetRequired` and requires
explicit reset or recreation. No storage reader, migration, backfill, dual
write, or census path survives. This plan authorizes no live-store inspection.
Pure source-only/internal request helpers and wire-visible V2 request revisions
may converge in place.
`V1` may identify an initial final wire record.

**Status (2026-07-23):** Landed on this branch. PR7/Plan 13's core — `RetrievalAnchorId`
identity and resolution, the branch's V2/V3-named anchor targets, native Git/worktree/integration-receipt
topology anchors, the immutable evidence-span/occurrence/retriever-contribution
contract, dispositions and safe tombstones, and the atomic evidence-assembly store — is
implemented across `crates/tracedecay-domain/src/research/`,
`crates/tracedecay-store/src/{evidence_assembly,retrieval_anchor}.rs`,
`crates/tracedecay-rusqlite-runtime/src/repository/evidence_assembly.rs`,
`crates/tracedecay-usecases/src/evidence_assembly.rs`, and `crates/tracedecay-runtime-core/src/db/retrieval_anchor_authority.rs`.
Per-section verdicts follow.

**Status split (2026-07-26, closed 2026-07-29).** The core above is delivered
PR7 behavior. Dedicated `GitHubStackCapabilitySnapshotV1`/`GitHubStackSnapshotV1`
anchor targets were the one named PR13/Plan 37 integration follow-up; they now
ship in `crates/tracedecay-domain/src/research/git_topology.rs` as
`GitTopologyAnchorTargetV1::{GitHubStackCapability, GitHubStackSnapshot}`.

Scope of that closure, stated exactly: this delivers Plan 13's anchor-target and
lineage contract, not a producer. Plan 27's stacked-pull-request capability
probing and Plan 37's PR15 read-only stack adapter remain unshipped, so no
production path mints these anchors yet. The targets exist so those owners bind
to one anchor contract instead of inventing a parallel reference family; their
own absence must not be refiled as a Plan 13 gap or as a PR14 dashboard gap.

## Outcome

Any authorized result can lead back to the exact retained observation or entity that
supports it. The reference survives ranking changes, project moves, worktree removal,
and index rebuilds, while deletion and retention remain explicit.

**Status (2026-07-23):** Implemented. Anchor IDs are derived from owner+target identity
(never rank, path, or payload bytes), so they survive ranking, project moves, worktree
removal, and index rebuilds; resolution reports
`current`/`drifted`/`redacted`/`expired`/`deleted`/`unavailable`/`ambiguous` with
coverage (`crates/tracedecay-domain/src/research/{anchor,resolution}.rs`,
`crates/tracedecay-runtime-core/src/db/retrieval_anchor_authority.rs`).

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
- Immutable bindings for repository/worktree captures, branch/ref snapshots,
  native commit/tree/blob objects, pull-request and check snapshots, conflict
  snapshots, GitHub stack capability/snapshots, and native-integration
  preflight/terminal receipts. These bindings carry
  only exact native identities and authorized receipt references; they never
  copy Git objects, patches, GitHub bodies, CI logs, or host-local summaries.
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

**Status (2026-07-23; completed 2026-07-29):** Implemented. `RetrievalAnchorId`
identity/resolution, provenance relations, evidence-time/generation/watermark/
coverage/drift state, immutable Git-object and repository/worktree/ref/PR/check/
conflict/preflight/integration-receipt bindings, safe tombstones, derived
evidence-span identity, and payload-free retriever-contribution anchors all exist
(`crates/tracedecay-domain/src/research/{anchor,git_topology,resolution,coverage}.rs`,
`crates/tracedecay-store/src/{evidence_assembly,retrieval_anchor}.rs`). The shipped
`GitTopologyAnchorTargetV1` covers `RepositoryCapture`/`WorktreeCapture`/`RefSnapshot`/
`NativeObject`/`PullRequestSnapshot`/`ReviewSnapshot`/`CheckSnapshot`/
`GitHubStackCapability`/`GitHubStackSnapshot`/`ConflictEvidence`/`PreflightPreview`/
`ApplyReceipt`/`IntegrationReceipt`.

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
- Repository/worktree discovery, Git object/ref interpretation, PR/check
  provider ingress, conflict handling, preflight computation, integration
  authorization, or Git application. Plan 36 owns native Git identity and
  typed preflight/apply/receipt semantics; Plan 24 owns task decisions; Plan
  32 owns admitted runtime effects; Plan 27 owns host/forge capability probing
  and transport conformance; and Plan 37 owns read-only GitHub stack snapshots.
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
   covers observation anchors; the read-only GitHub review and CI localization
   journey adds those evidence classes. Retry returns the existing anchor.
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
17. A worktree, branch, check, conflict, preflight, or integration anchor denotes one
    immutable observation or terminal receipt, never a mutable checkout, ref, PR,
    task, host capability, or permission. Capturing a later `HEAD`, branch target,
    check rerun, conflict state, or receipt creates a new target and explicit lineage;
    it never retargets an existing anchor.
18. Native Git object targets contain the canonical `RepositoryId`, native object
    format, object kind, and exact object ID from Plan 36's native capture. A branch,
    tag, symbolic ref, current `HEAD`, path, diff digest, GitHub label, or timestamp
    cannot substitute for that object identity.
19. Pull-request targets reference the exact Plan 36 `PullRequestSnapshot`;
    check targets reference the exact Plan 27-decoded, Plan 03-canonicalized
    observation consumed by Plan 37. They retain provider locators only as
    privacy-domain-bound digests. They do not duplicate GitHub review text,
    check output, CI logs, patch hunks, provider cursors, or mutable
    REST/GraphQL response payloads. GitHub stack targets separately reference
    the exact Plan 27 `GitHubStackCapabilitySnapshotV1` and Plan 37
    `GitHubStackSnapshotV1`; provider stack ID, position, base/head refs, and
    final-target identity never become generic branch-stack or task identity.
20. Conflict and preflight targets reference an exact Plan 36 `RepositorySnapshot`,
    unmerged-stage or preflight receipt digest, normalized operation/options digest,
    and policy/configuration revision. They are read-only evidence and cannot be
    replayed as an apply instruction.
21. Integration-receipt targets bind a typed request, exact preflight anchor,
    repository/worktree/branch/commit anchors, policy decision, and the owning
    Plan 24 decision and Plan 32/36 terminal receipts. Plan 13 stores no merge
    command, patch, credentials, host command line, or authority grant, and
    receipt possession never authorizes an integration action.
22. Task, worktree, and native-integration summaries retain an ordered, lossless
    `RetrievalAnchorId` lineage to every exact source. A Plan 23 summary or Plan 24
    task projection may abbreviate presentation, but its prose, score, status, branch
    label, or aggregate cannot become replacement evidence.

**Status (2026-07-23; item 19 completed 2026-07-29):** Items 1–22 implemented.
Shipped reality:

- Item 1: the opaque `RetrievalAnchorId` is realized as a derived, digest-tagged
  string — `retrieval.v2.sha256:<hex>` for observation/repository/entity targets and
  `retrieval.v3.sha256:<hex>` for Git-topology and the V3 occurrence/span/contribution
  targets (`derive_anchor_id`/`derive_v3_anchor_id`,
  `crates/tracedecay-domain/src/research/anchor.rs`). The digest is over owner+target
  identity, never payload bytes, so "not a content hash" still holds. A legacy bare
  `sha256:<hex>` form persists in older stored rows and payload-integrity checks
  (`crates/tracedecay-global-db/src/session_temporal/hydration.rs`).
- Item 5: the seven states are `AnchorResolutionStateV2`
  (`crates/tracedecay-domain/src/research/resolution.rs`).
- Coverage: derived-group (Span/Burst) candidate anchors are excluded from the coverage
  denominator so grouped members are not double-counted as hidden omissions
  (`src/query/temporal/mod.rs`; landed as commit `67a7f253`).
- Items 11–18, 20–21: native repository/worktree/ref/object/PR/check/conflict/preflight/
  integration anchors ship in `crates/tracedecay-domain/src/research/git_topology.rs`.

## Native Git, worktree, and integration-receipt anchors

PR7 extends `RetrievalAnchorTargetV3`; it does not create a parallel public
topology ID family. Plan 16 owns canonical project/repository/worktree
relationships, Plan 36 owns native Git capture and object/ref interpretation,
Plan 27 owns provider/host observation decoding into Plan 03 canonical
observations, Plan 24 owns task decisions and task-to-evidence links, and Plan
32 owns admitted effects and terminal runtime receipts. Plan 13 stores
immutable, payload-free references to those owner records.

The public V3 anchor target retains one Git-topology variant beneath
`RetrievalAnchorId`. It can reference repository and worktree captures, ref
snapshots, native objects, pull-request/check snapshots, conflict and preflight
observations, and integration receipts. These persisted wire records use the
canonical identities owned by Plans 03, 16, 20, 27, 32, and 36; Plan 13 does
not derive substitutes or create a parallel public lookup family.

Those records contain only exact owner references, immutable object/snapshot
identity, configuration/policy bindings, ordered source-object anchors, and
typed receipt identities and digests. They contain no copied Git object,
patch, provider body, CI log, task record, command line, credential, grant, or
host summary. Task-linked integration evidence requires the owning task
decision reference; non-task observations leave it absent. Validation rejects
owner/privacy/repository/capture/object-format mismatch, cross-repository
native apply, destinations outside the bound capture, changed source sets, and
owner receipts whose identity or digest does not match the referenced record.
These are product-runtime provenance receipts from the owning operation, not
PR acceptance owner receipts or planning evidence.
Historical Rust type and source-file names remain evidence for wire migration,
not an implementation layout requirement.

`RepositoryCaptureAnchorRefV1` identity is the Plan 16 `RepositoryId` plus the
exact Plan 36 native repository snapshot identity, object format, and common-Git-
directory identity. `WorktreeCaptureAnchorRefV1` identity additionally binds the
Plan 16 `WorktreeId`, native linked-worktree administrative identity, captured
HEAD/index state, and capture receipt. A path, CWD, remote URL, branch name,
inode, timestamp, matching `HEAD`, copied object bytes, or content hash is never
capture identity. A native object anchor is valid only when Plan 36 proved that
object readable in the named capture. Later object loss yields `unavailable`;
resolution never consults an ambient ref to replace it.

A branch or tag anchor is one `RefSnapshotAnchorRefV1`, not the mutable ref.
Movement creates another target with lineage to the prior ref snapshot. Commit,
tree, and blob anchors bind native object kind, native object format, object ID,
and repository capture. SHA-1 and SHA-256 IDs are never compared without their
object-format and repository-capture bindings.
`target_object_anchor_id` is absent only for a Plan 36-proven unborn symbolic
ref; direct refs and attached symbolic refs require the exact object anchor.

Moving a checkout root or reaching it through a symlink changes routing only
after a fresh Plan 16/36 resolution proves the same `RepositoryId`,
`WorktreeId`, and `NativeWorktreeAdminId`. The resolver then appends a
`MovedFrom` or `ResolvedViaSymlink` locator observation with a privacy-domain-
bound locator digest. Path-string equality, matching `HEAD`, inode reuse,
matching remotes, or symlink target text cannot prove continuity. A symlink
escape, ambiguous native admin identity, or missing proof returns
`ambiguous`/`unavailable`; it never rekeys the old anchor or reveals the prior
raw path.

**Status (2026-07-23; completed 2026-07-29):** Implemented.
`GitTopologyAnchorTargetV1` exposes `RepositoryCapture`, `WorktreeCapture`,
`RefSnapshot`, `NativeObject`, `PullRequestSnapshot`, `ReviewSnapshot`,
`CheckSnapshot`, `GitHubStackCapability`, `GitHubStackSnapshot`,
`ConflictEvidence`, `PreflightPreview`, `ApplyReceipt`, and `IntegrationReceipt`
variants with capture/object-format/receipt bindings; SHA-1/SHA-256 IDs carry
object-format and repository-capture bindings
(`crates/tracedecay-domain/src/research/git_topology.rs`).

`GitHubStackCapabilitySnapshotV1` records the exact four-state Plan 27
capability observation; `GitHubStackSnapshotV1` exists only for an `Enabled`
capability and validates a same-repository, strictly linear layer chain whose
lowest layer sits on the declared final target. Each layer reuses the exact
`PullRequestSnapshotAnchorRefV1` rather than duplicating pull-request identity,
the provider stack ID is retained only as a `PrivacyDomainBoundLocatorDigest`,
and both records exclude review text, check output, CI logs, patch hunks, and
provider cursors. A later provider observation rekeys to a new target instead of
retargeting the retained one.

### Lossless `TaskId` and integration drilldown

Plan 24's existing `TaskEvidenceLinkRevision` is the only task-to-anchor edge.
Plan 13 does not store `TaskId`, work-item status, task labels, readiness, or
acceptance. A task or integration summary uses `AnchorLineageRefV3` with a
strictly ordered `source_ordinal`; its constructor requires every immediate
source anchor and rejects prose, scores, branch labels, statuses, timestamps,
provider text, CI output, patches, or aggregate digests as source evidence.

```text
TaskId (= WorkItemId)
  -> Plan 24 WorkItemVersionId and TaskEvidenceLinkRevision
  -> Plan 13 RetrievalAnchorId / ordered AnchorLineageRefV3
  -> repository/worktree/ref/object/PR/check/conflict/preflight anchors
  -> Plan 24 decision, Plan 32 runtime, and Plan 36 native-operation receipts
  -> current-authorized owning record or safe disposition
```

Every page, expansion, and hydration reauthorizes each hop. A missing,
redacted, deleted, expired, drifted, ambiguous, or unavailable source remains a
typed omission; a summary cannot collapse it into success. Receipt possession
grants no policy, runtime, Git, provider, or host authority.

**Status (2026-07-23):** Implemented. `AnchorLineageRefV3` carries child/source anchor
IDs, a strictly ordered `source_ordinal`, and explicit owner/privacy binding, and its
validation (`validate_anchor_lineage_v3`) rejects prose/score/label sources
(`crates/tracedecay-domain/src/research/anchor.rs`). Task-to-anchor edges stay Plan 24's
`TaskEvidenceLinkRevision`; Plan 13 stores no `TaskId`.

### Persistence and direct acceptance

Domain validation owns identity and closed errors; the store owns append-only
publication and resolution; the application reauthorizes and resolves owner
receipts; and the infrastructure adapter owns persistence. None invokes Git,
GitHub, CI, host processes, task mutation, or workflow execution.

Persisted records remain immutable, owner/privacy-bound, payload-free, and
ordered where source membership is meaningful. The exact final persisted shape
is the sole admission format. Any other stored shape returns `ResetRequired`
before interpretation and requires explicit reset or recreation; it is never
read, migrated, backfilled, dual-written, or retained pending a census. Public
wire/API compatibility is separate and applies only to actual independent
releases.

Direct tests prove deterministic IDs, repository/object-format separation,
exact object and receipt binding, ref-movement rekeying, ordered membership,
atomic publish/replay/rollback, immutable records, authorization/disposition
parity, moved and symlinked worktree non-inference, SHA-1/SHA-256 separation,
exact PR/check/preflight/integration evidence, TaskId-rooted drilldown with
typed omissions, exact-final-shape admission with `ResetRequired` refusal, and
absence of copied or payload-bearing owner data. Historical schema, index,
trigger, and test-file names are not mandatory recreation targets.

**Status (2026-07-23):** Implemented. Domain records own identity/validation, the store
owns publish-or-replay and payload-free resolution, and the rusqlite runtime repository
persists append-only immutable rows with replay-conflict detection
(`crates/tracedecay-store/src/evidence_assembly.rs`,
`crates/tracedecay-rusqlite-runtime/src/repository/evidence_assembly.rs::insert_immutable`).
Direct coverage: `tests/session_suite/{anchor_resolution,anchor_tombstone_expiry}.rs` and
`crates/tracedecay-store/tests/session_contract/`.

## Immutable evidence-span contract

PR7 adds one payload-free identity model for consecutive message, tool-invocation,
tool-result, and code-chunk occurrences. It does not reuse observation-level anchors,
`SessionSummaryRecordV1::source_anchors`, or sorted `RetrievalAnchorRecordV2`
lineage as an ordering model: those collections cannot distinguish multiple projected
outputs from one observation or preserve assembly order.

The persisted V3 contract retains one profile identity type; opaque occurrence,
set, span, contribution, disposition, replay, and privacy-bound request
identities; explicit owner binding for child and source lineage; immutable
source occurrence and evidence-span records; retriever contribution and
watermark records; and disposition, tombstone, and resolution states. It does
not decode a non-final persisted V2 row: that row returns `ResetRequired`
before interpretation. A project may be absent only for
explicitly profile-owned evidence; path, CWD, store filename, project label,
host profile, PID, branch, or ref can never fill owner identity. Historical
Rust module and helper-type names do not constrain the current implementation.

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
source-ordered members. The application evidence verifier is the only authority
that can produce a verified source-ordering proof; it checks the Plan 08 catalog digest, Plan 27
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
`RetrieverContribution(RetrieverContributionIdV1)`. `RetrievalAnchorRecordV3`
is the final persisted shape; older stored V2 rows return `ResetRequired`
rather than being extended, decoded, or converted.
`derive_exact_source_occurrence_anchor_id`,
`derive_exact_evidence_span_anchor_id`, and
`derive_retriever_contribution_anchor_id` call the existing canonical digest
machinery. Public lookup still uses only `RetrievalAnchorId`; the new IDs identify
immutable targets and do not create a parallel public reference family.

**Status (2026-07-23):** Implemented, with the type-name drift this plan already
disclaims. The store defines `EvidenceSourceOccurrenceRecordV1` (plan
`SourceOccurrenceRecordV1`), `CanonicalSourceOccurrenceSetRecordV1` (plan
`CanonicalSourceOccurrenceSetV1`), `EvidenceSpanRunV1`, `EvidenceSpanRecordV1`,
`EvidenceSpanHorizonV1`, `SourceCapabilityCatalogBindingV1`, and
`VerifiedSourceOrderingProofV1`; `SourceOccurrenceSanitizationV1::new(capture,
projection)` keeps the two receipt roles distinct; `RetrievalAnchorTargetV3` adds the
three exact targets and the `derive_exact_*` functions mint `retrieval.v3.sha256:` IDs
(`crates/tracedecay-store/src/evidence_assembly.rs`,
`crates/tracedecay-domain/src/research/anchor.rs`). An application-layer mirror
(`SourceOccurrenceRecord`/`CanonicalSourceOccurrenceSet`/`EvidenceSpanRecord`) lives in
`crates/tracedecay-usecases/src/evidence_assembly.rs`. The plan's `PublishEvidenceAssembly::execute` is
realized as the store trait method `EvidenceAssemblyStore::publish_or_replay`.

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

**Status (2026-07-23):** Implemented. `RetrieverContributionRecordV1`,
`RetrieverIdentityV1`, `RetrieverWatermarkBindingV1`, `PrivacyBoundRequestDigestV1`,
`EvidenceAssemblyWriteV1`, and `EvidenceAssemblyIdempotencyKeyV1` ship in
`crates/tracedecay-store/src/evidence_assembly.rs`; `publish_or_replay` returns the
existing receipt on identical scoped material and `ReplayConflict` on changed material.
A contribution stays explanation-only evidence — rank/score/query/embedding cannot
retarget it.

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

All new durable lineage uses `AnchorLineageRefV3` with child/source anchor ID
and explicit canonical owner/privacy binding. A stored V2 row that is not the
exact final shape returns `ResetRequired` and cannot serve, decode, or convert.
Publication atomically writes forward and reverse lineage. A provider-native copied message remains a distinct source
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
tombstone or existence distinction. Final-shape restore, consolidation, and
replay apply current dispositions before rebuilding derivatives, so stale copies
cannot resurrect payload access.

**Status (2026-07-23):** Implemented. Owner-bound create/resolve, append-only
`RetrievalAnchorDispositionRecordV1`, and the strict-whitelist
`RetrievalAnchorTombstoneV1` ship in `crates/tracedecay-store/src/retrieval_anchor.rs`
and are enforced by `crates/tracedecay-runtime-core/src/db/retrieval_anchor_authority.rs` (disposition-transition
rules, derivative suppression, newest-disposition-first resolution). `AnchorLineageRefV3`,
`LogicalCopyRecordV1`/`CopiedFrom`, and `SessionSummaryRecordV1` provide copy/summary
lineage (`crates/tracedecay-domain/src/{research/anchor,session}.rs`). Tombstone-expiry
and revocation are exercised by `tests/session_suite/anchor_tombstone_expiry.rs`.

## Persistence behavior

Domain records own immutable identity and structural validation; the store
owns atomic publish-or-replay and payload-free resolution; application
operations own current authorization, sanitization-receipt and source-order
verification, and transactional orchestration; infrastructure owns physical
persistence and dispositions-first consolidation. No layer copies snippets,
summary or FTS text, hydrated payloads, query text, paths, arguments, results,
or embeddings into evidence-assembly records.

Persisted occurrence, canonical-set, span, projection-receipt, contribution,
lineage, disposition, and replay records accept only the exact final shape.
Any other stored shape returns `ResetRequired` and requires explicit reset or
recreation; no compatibility reader, migration, backfill, dual write, or
census applies. Final records enforce owner/privacy binding, immutable
membership and order, referential integrity, append-only dispositions,
canonical digests, and efficient reverse resolution. Physical table, column,
index, trigger, and source-file names are implementation history rather than
features to recreate.

Admission rejects every non-final or unsupported stored shape with
`ResetRequired` before any read, write, replay, or projection. Final-shape
records verify owner/privacy binding, source order, receipt roles, flattened
span/set equality, catalog and integration bindings, projection-receipt
membership, deterministic digest replay, dispositions, atomicity,
authorization, replay-conflict, tombstone-whitelist, and payload-free
persistence. Failure blocks admission without re-enabling deleted or redacted
payloads.

**Status (2026-07-23):** Implemented. The four-layer split — domain validation, store
publish-or-replay, application authorization/orchestration
(`crates/tracedecay-usecases/src/evidence_assembly.rs`), and infra persistence
(`crates/tracedecay-rusqlite-runtime/src/repository/evidence_assembly.rs`) — is in place
with immutable inserts and atomic rollback on replay conflict.

## Cross-plan ownership

- Plan 13 owns `RetrievalAnchorId`, V3 anchor targets and lineage,
  occurrence-set/span/
  contribution identity, owner-bound lineage, replay semantics, resolution states,
  dispositions, and minimum-safe tombstones. Plan 02 owns generic persistence and
  final-shape persistence policy/primitives; Plan 13's PR7 owns the evidence-assembly
  persistence contract and adapter behavior above. Plan 09 owns current authorization and
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
- Plan 20 owns topology configuration and policy digests; Plan 32 owns runtime
  admission, effects, and terminal receipts; and Plan 36 owns native Git
  repository/worktree/ref/object identity, snapshots, preflights, and admitted
  Git-operation receipts. Plan 13 anchors exact owner records and ordered
  lineage only. It cannot turn a policy revision, task decision, preflight,
  receipt, or anchor possession into mutation authority.
- Plan 35 owns diagnostic identity projection through the daemon LSP gateway,
  while Plan 37 owns feedback, review/CI advisory findings, and proximity
  semantics. Either may cite Plan 13 anchors; neither may copy topology
  evidence into an LSP/host payload as replacement authority, and Plan 13 does
  not redefine their finding or delivery models.
- Plan 08 owns callable source-capability definitions, stable `CapabilityId`, and
  `CatalogDigest`. Plan 27 owns host adapters, provider-native ordering evidence,
  `HostIntegrationManifestV1`, source connector/root bindings,
  `PlannerSourceDescriptorV1`, and projector revision. Plan 13 persists only
  `SourceCapabilityCatalogBindingV1` and validates both authorities; it does not
  define capabilities or host semantics. Host files and processes may transport
  anchor IDs but cannot mint authorization, resolve stores locally, persist anchor
  copies, sanitize independently, or infer owner identity from ambient host state.

**Status (2026-07-23):** Consistent with shipped layering. Plan 13 code references Plan
24 (`TaskEvidenceLinkRevision`), Plan 08/27 (`SourceCapabilityCatalogBindingV1`), and
Plan 36 native identities without redefining them.

## Lossless evidence boundary

Durable products resolve through `RetrievalAnchorId` plus owning-store retention
for sanitized payloads. [Plan 05](05-query-crate.md) opaque cursors page typed
collections only. Transport `rh_` response handles from
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) are 24-hour,
project-local output recovery for truncated MCP/CLI responses and are never
durable evidence identity, anchor targets, or storage keys. This plan does not
own response-handle implementation.

Read-only GitHub thread/comment/reply and CI-failure ingress may create and
resolve these anchors without [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)
as a prerequisite. Plan 32 is required only for admitted write-side effects and
workflow automation outside this contract.

**Status (2026-07-23):** Implemented. CI-failure and feedback ingress already
create/resolve anchors without Plan 32 (`crates/tracedecay-usecases/src/advisory/ci_runtime/`,
`crates/tracedecay-usecases/src/feedback/owner.rs`); `rh_` response handles remain Plan 21 transport
artifacts, not durable evidence identity. Anchor resolution is surfaced downstream by the
shipped dashboard provenance UI (`dashboard/src/ui/EvidenceTruthStrip.tsx`, Observatory
Doctor findings in `dashboard/src/workspaces/observatory/`), which passes anchor IDs
through without defining another reference type (Required behavior 10).

## Acceptance

- PR7 tests atomic observation-and-anchor creation, idempotent replay, rollback, native
  alias collisions, copied-prompt attribution, and unauthorized resolution.
- Rebuilding projections preserves anchor IDs and source lineage.
- Evidence-span contract tests prove deterministic
  source-occurrence/set/span IDs, mixed message/tool/code runs, exact ordering,
  cross-source assembly-only semantics, horizon validation, V3 wire compatibility,
  byte-identical canonical-set normalization across input permutations, catalog
  binding, same-timeline tool-result pairing, UTF-8/CRLF coordinate stability, and
  rejection of gaps, duplicates, owner/generation/timeline mismatch, bare offsets,
  content hashes, summaries, embeddings, rank, and query identity.
- Retriever-contribution contract tests prove exact
  replay, changed immutable-input rekeying, idempotency conflicts, source-set/span
  equality, owner/request privacy-domain and key-epoch equality, payload-free
  serialization, and independent Plan 08 catalog plus Plan 27 connector/root/
  manifest/configuration/authorization/projector/source-watermark tamper rejection.
- Evidence-assembly store tests prove one atomic
  set/span/contribution/anchor/lineage/receipt transaction, rollback on every
  conflict, immutable-table triggers, exact drill-down, authorization parity, and
  round-trip isolation for multiple rebuild receipts on two spans that share an
  occurrence; missing, extra, duplicate, and cross-span receipt members are rejected.
  The same raw idempotency key in two owner/privacy domains does not collide or reveal
  occupancy, while same-scope changed material returns `ReplayConflict`.
- Session projection and temporal-retrieval tests prove same-version
  rebuild identity, new-projector `DerivedFrom` lineage, verified adjacency,
  singleton boundary handling, ranking-independent replay, and contribution -> span
  -> set -> exact source expansion.
- Tombstone-expiry and summary-lineage tests prove strict tombstone fields,
  authorization revocation, possession-only denial, and transitive source deletion
  through span -> contribution -> nested summary -> FTS/context.
- Persistence tests prove exact-final-shape admission, typed `ResetRequired`
  refusal for every other shape, integrity and immutability constraints,
  final-shape restore/consolidation, rollback, and unconditional
  no-payload-resurrection safety. No storage reader, migration, backfill, dual
  write, or census path exists. Historical schema-object names are not
  acceptance artifacts.
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
  product behavior tests.
- The same native locator in two profiles/projects/privacy domains or key epochs
  produces unlinkable aliases, and an unauthorized caller cannot distinguish a
  tombstone from an unknown anchor.
- Repository search finds no research-ledger, plan-parser, compatibility-inventory, or
  plan-execution requirement in this contract.

**Status (2026-07-23; completed 2026-07-29):** Implemented. Acceptance coverage
lives in
`tests/session_suite/{anchor_resolution,anchor_tombstone_expiry,fact_anchor_authority,temporal_derived_evidence,temporal_privacy}.rs`,
`tests/session_suite/temporal_projection/lineage.rs`,
`crates/tracedecay-store/tests/session_contract/`, and
`crates/tracedecay-domain/src/research/{anchor_test,resolution}.rs`. GitHub-stack
target evidence (Required behavior 19) is covered by
`crates/tracedecay-domain/tests/git_topology_anchor_contract.rs`, which proves
capability/snapshot generation binding, ordered layer lineage, anchor rekeying on
a later observation, payload-free retained records, and rejection of non-enabled
capabilities, empty layers, detached bases, mis-numbered positions,
cross-repository layers, and a final target the lowest layer does not sit on.
