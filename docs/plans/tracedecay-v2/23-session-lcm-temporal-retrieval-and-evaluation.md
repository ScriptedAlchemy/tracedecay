# TraceDecay V2 Session and LCM Temporal Retrieval

**Delivery:** PR 8

**Status:** completed PR8 product delivery and retained temporal retrieval authority.
**Depends on:** [01 domain](01-domain-crate.md), [02 store](02-store-crate.md), [03 capture](03-capture-crate.md), [04 projectors](04-projectors-crate.md), [05 query](05-query-crate.md), [09 application](09-application-crate.md), [13 anchors](13-research-provenance-and-context-anchors.md), and [18 privacy](18-secret-detection-redaction-and-private-data-safety.md). PR8 implements against explicitly resolved current-project/single-root scope and address contracts available by then; the [multi-root scope plan](16-cross-project-repository-worktree-scope.md) later composes this same retrieval kernel with canonical cross-project/repository/worktree resolution and is not a PR8 implementation prerequisite.

**Retention ownership correction (2026-07-26).** Plan 23 owns retrieval-time
retention-expiry rechecks and truthful `RetentionWithheld` coverage reporting.
Plan 38 owns raw LCM offload/drop, projected-copy dedupe, observation-evidence
release, and any future `source_cursor_advances` reclamation. Missing retention
writers or immutable-table changes are not unfinished PR8 work.

Plan 23 remains the owner of the behavior in this document. Plans 15, 24, and
37 and the application/public-surface plans are later consumers. They must
 reuse PR8's accepted behavior; they do not have
to recreate PR8's original module tree, Rust type spellings, schema names,
suite registration, fixture filenames, benchmark scripts, or command list.

The session-temporal schema versions 2 and 3 were introduced on this integration
branch. They use the V2 fresh-store cutover: only the exact final persisted
shape is accepted. Any other database, store, spool, file, or projection
returns typed `ResetRequired` and requires explicit reset or recreation. No
storage reader, migration, backfill, dual write, or census path exists. This
plan authorizes no live-store inspection; `session_messages` and `lcm_*` are
not upgrade sources.

## Outcome

PR 8 replaces fragmented message search and LCM lookup with one temporally correct retrieval path for messages, Turns, sessions, threads, agents, and summaries. It returns the smallest useful context while preserving exact text, history, provenance, privacy, and stable anchors.

This is product retrieval work. It does not implement task filtering, plan execution, a benchmark bureaucracy, or a Search Quality Lab.

## Evidence authority boundary

LCM external payloads and the summary DAG are canonical only for session-linked
narrative and tool-output context: messages, Turns, sessions, threads, agents,
and derived summaries over that evidence.

They may reference [Plan 13](13-research-provenance-and-context-anchors.md)
`RetrievalAnchorId` values and provide bounded drill-down to exact retained
evidence, but they never become canonical authority or durable storage for:

- GitHub review threads, comments, or replies;
- CI runs, logs, or artifact excerpts;
- diagnostics or provider findings;
- Git snapshots, `HunkRef`, or mutation receipts; or
- workflow/effect receipts.

A summary cannot replace or hide exact evidence. When a query needs GitHub, CI,
diagnostic, or Git evidence, resolution goes through Plan 13 anchors and the
owning store for that evidence class.

Transport `rh_` response handles from
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) are 24-hour,
project-local output recovery for truncated MCP/CLI responses. They are not
durable evidence identity and must not be stored as canonical LCM or summary
sources. [Plan 05](05-query-crate.md) opaque cursors page typed collections only.

## Source truth

- Every provider observation is an immutable `MessageOccurrenceRecordV1` with
  canonical source identity, provider order, `knowledge_at: UtcMicros`,
  `valid_time: TemporalValidityV1`, resolved scope, and sanitization receipt.
- `knowledge_at` records when sanitized evidence became visible to the
  authoritative store or projection. Provider timestamps never substitute for
  it.
- `valid_time` independently records when evidence applies in the represented
  world as `Known { valid_at }` or `Unknown`; ingest order never fabricates it.
- Logical copies are versioned `LogicalCopyRecordV1` relations backed by origin
  evidence. Hashes, timestamps, titles, exact text, or embeddings alone never
  collapse independent repetitions.
- Corrections, supersession, and contradiction append immutable
  `TemporalAssertionRecordV1` values. Reusing an assertion ID with different
  canonical content is an idempotency conflict.
- Turns and threads are persisted retrieval grains. Raw occurrences remain
  addressable subject to authorization, retention, redaction, and deletion.
- Ready and active generations reject UPDATE and DELETE of occurrences,
  assertions, copy/supersession edges, derived evidence, summaries, and their
  manifests. Rebuilds write and atomically activate a new generation.

The normative domain contract is the callable behavior above: independent
knowledge and valid time, immutable occurrences and assertions, explicit copy
evidence, summary source horizons, and truthful coverage. Historical type and
module names are provenance for the original delivery, not parity targets for
a later implementation.

## Immutable derived evidence spans and bursts

PR8 adds actionable evidence spans and bursts as immutable, generation-scoped,
rebuildable derived projections over consecutive message and tool-call
occurrences:

- a span is a bounded actionable interval selected by a versioned policy;
- a burst is a maximal consecutive run selected by a versioned adjacency
  policy;
- membership follows canonical
  `(observation_sequence, projection_output_ordinal)` order, never timestamps,
  titles, similarity, or model output;
- neither is source authority, a summary, a replacement occurrence, or an
  external evidence store.

Each derived span or burst binds a typed identity and retrieval anchor, session
and optional thread identity, first and last occurrence identity, member count
and digest, algorithm/configuration revision, source horizon, and an explicit
derived-projection authority class. It stores no copied message, tool payload,
concatenated span text, GitHub, CI, diagnostic, Git, receipt, task, or `rh_`
payload. Each derived item is an anchored entity with one `DerivedFrom`
lineage edge for every member occurrence anchor.

The projection rejects cross-session or cross-generation members, duplicate or
noncontiguous ordinals, and endpoint/manifest mismatches. The derived ID and
member digest bind kind, algorithm/configuration version, and the complete
ordered occurrence-ID list. Identical frozen input produces byte-identical
derived records, manifests, anchors, indexes, and receipt digests.

Authorized drill-down pages the complete ordered member manifest. Deleted,
redacted, expired, locked, retained-but-unavailable, or unauthorized members
occupy their original ordinal as typed omissions; they never disappear or
promote replacement evidence. A summary citing a span or burst is eligible only
while every leaf occurrence anchor remains authorized and available.

## Temporal modes

The shared query accepts four explicit modes:

- `current`: apply eligible `Corrects`, `Supersedes`, and `Contradicts`
  assertions only when authority and supported evidence strength justify
  suppression; otherwise retain both sides as a material conflict.
- `as_of { cutoff }`: independently require `knowledge_at <= cutoff` and known
  `valid_at <= cutoff` for every occurrence and assertion before applying
  current-mode authority rules. Unknown valid time is excluded from
  representative answers.
- `evolution`: retain eligible versions in correction/supersession graph order,
  not incidental timestamp order; cycle members remain visible and conflicted.
- `forensic`: preserve every authorized occurrence, explicit logical copy,
  assertion, unknown valid time, and uncertainty without current-answer
  suppression.

Recency is bounded evidence, not a truth rule. A newer weak mention does not erase an older authoritative decision, and an old exact match does not silently override a supported correction.

Freshness is mode-specific. `current` compares each selected source frontier
with the pinned current target. `as_of` reports historical coverage at the
cutoff and cannot leak later ingest or correction. `evolution` reports covered
and missing source intervals for the returned lineage. `forensic` reports all
authorized source states and unknown validity without collapsing mixed
coverage to empty-complete.

## Retrieval pipeline

This is the sole temporal retrieval kernel. Only independently released
`message_search` and `lcm_grep`/load/describe/expand/query protocols may
translate into this request and delegate; they do not keep separate ranking,
hydration, context, pagination, or freshness logic. `src/mcp/server.rs`,
`src/mcp/tools/handlers/session/message_search.rs`, and
`src/mcp/tools/handlers/session/lcm_handlers/mod.rs` are translation/rendering
adapters only: they do not query LCM tables, call `get_session_message`,
hydrate payloads, apply semantic filters after ranking, or encode a second LCM
cursor. Workflow recovery consumes session evidence through this kernel; the
executable-work product owns all other workflow semantics.

PR8 accepts exactly one already-resolved `ResolvedSessionIdentity` and one
`SessionRetrievalScope::{Session, AllSessionsInAuthorizedRoot}` per request.
`AllSessionsInAuthorizedRoot` means authorized sessions in that identity's
existing store/root. PR8 performs no registry enumeration, CWD-derived store
selection, `all_registered` fan-out, cross-project composition, or store
switching.

The kernel executes in this order:

1. `SessionRetrievalService::retrieve` resolves authorization and puts query
   text, every semantic filter, provider/session selector, grain, temporal mode
   and cutoff, budget, ranking/diversity versions, and policy digest into one
   canonical request.
2. `SessionTemporalExecutionPort` freezes a sorted manifest for every
   participating session/source: store generation and source, projection,
   graph, index, summary, configuration, and authorization watermarks.
3. `TemporalReadPort` emits compact exact-literal, lexical, phrase, fuzzy,
   entity, span, burst, summary, graph, time, and configured semantic candidates
   without reading full payloads.
4. `resolve_temporal_with_checkpoints` resolves copies, assertions, authority,
   contradictions, and the requested temporal mode. Contradiction admission
   occurs before duplicate rejection.
5. `rank_candidates` fuses calibrated contributions under one stable evidence
   identity, then deduplicates and diversifies.
6. The kernel freezes the ordered page and continuation boundary before reading
   payload bytes.
7. `TemporalHydrationPort` authorizes and rechecks each selected anchor and
   returns one positional available or typed-unavailable result per rank.
8. Context assembly emits selected Turns/derived intervals, exact support,
   summary lineage, conflicts, omissions, source coverage, and continuation
   anchors under the exact byte/token budget.

### Exact literals, fusion, deduplication, and diversity

Exact-literal retrieval performs a byte/codepoint-exact predicate against
sanitized occurrence text and records the matching occurrence plus exact byte
ranges. It matches a literal embedded in a longer occurrence; whole-snippet
equality and tokenizer output are not correctness gates. FTS may accelerate
the predicate but cannot suppress a match. Exact identifiers,
punctuation-heavy errors, paths, symbols, commands, quoted phrases, CJK, and
emoji outrank generic semantic neighbors.

`crates/tracedecay-domain/src/session.rs` defines `RetrieverIdV1`,
`SourceDiversityKeyV1`, `ByteRangeV1`, and `RetrieverContributionV1`.
`src/query/temporal/candidates.rs` owns exact-literal and span/burst candidate
production; `src/query/temporal/ranking.rs` consumes the domain types and owns
fusion. Every contribution records retriever, canonical provider/source
partition, retriever ordinal, raw and calibrated score, matching occurrence
ID, and exact ranges. Candidate identity is the evidence identity, independent
of retriever/channel, so one item found by several retrievers becomes one
candidate retaining every contribution. Uncalibrated shard-local scores are
never compared directly.

Dedupe and diversity run after temporal/conflict admission. Only explicit
`LogicalCopyRecordV1` evidence collapses occurrences; independent repetitions
remain addressable. Diversity uses canonical logical-message, Turn, thread,
session, provider/source, and evidence-role keys. Source identity is never an
occurrence or summary ID. Every deterministic exclusion remains in the score
explanation as a typed diversity decision.

### Rank-final authorized late hydration

`src/query/temporal/hydration.rs` owns `TemporalHydrationPort`,
`RankedHydrationResultV1`, and
`RankedHydrationPayloadV1::{Available { payload },
Unavailable { reason: HydrationStateV1 }}`; the unavailable reason reuses the
existing domain `HydrationStateV1`.
`TemporalHydrationPort` receives an immutable ordered ranked page and returns a
`Vec<RankedHydrationResultV1>`. Every result repeats the selected `rank`,
stable evidence ID, and anchor ID; ranks are unique, contiguous for the page,
and in input order. The result contains no score or reorder operation.

Hydration may reveal bytes or change an item from available to unavailable
after authorization recheck. It cannot add, remove, reorder, promote, demote,
backfill, replace, or change the cursor boundary. Authorization drift preserves
the denied anchor's rank as an omission. `src/query/temporal/context.rs`
consumes only these positional results for payload-derived snippets, support,
and omissions; it cannot backfill from candidate metadata. No handler,
renderer, or context assembler performs a second payload lookup.

### Canonical pagination

`src/query/temporal/cursor.rs` owns the only continuation cursor used by search,
direct-anchor lookup, describe, expansion, expand-query, hydration, replay, and
legacy LCM adapters. MAC verification precedes binding diagnostics.
This is a bounded integrity check at the concrete untrusted-continuation
boundary, not a first-party signature, trust root, attestation, or acceptance
artifact.

The cursor binds canonical query text and every semantic filter, resolved
authorization/root/provider/session scope, grain, temporal mode/cutoff,
ranking/diversity/projector/configuration/signing-key-route versions, stable
evidence sort key/page boundary, and the sorted per-session/per-source manifest
of store, source, projection, graph, index, summary, configuration, and
authorization watermarks.

Continuation rejects any binding or watermark drift, including a non-anchor
session/source change during root-wide retrieval. Payload availability and
bytes are not cursor inputs. The offset-only LCM cursor is removed, not
strengthened as a parallel format.

`crates/tracedecay-domain/src/session.rs` defines
`SessionFrozenSourceWatermarkV1` as one canonical manifest entry and
`SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS = 256`. Entries sort by
`(session_id, source_id)` and duplicate keys are invalid. The uncompressed
canonical cursor payload is capped at 65,536 bytes. A request exceeding either
bound returns
`SessionRetrievalOutcome::CursorManifestLimitExceeded {
kind: CursorManifestLimitKindV1, observed, maximum }`, where
`CursorManifestLimitKindV1::{Participants, CanonicalBytes}` distinguishes the
256-participant and 65,536-byte limits. Rejection occurs before candidate
generation and persists no cursor state.

`crates/tracedecay-global-db/src/session_temporal/cursor_keys.rs` owns cursor-key provisioning,
rotation, and retention. A cursor expires 24 hours after issue; retired keys
remain verification-only for 24 hours plus five minutes of clock skew. Reads
never provision or rotate keys. The untrusted key-route ID may select a retained
verification key, but no scope/binding detail is returned until MAC
verification succeeds. Unknown or expired routes return only
`CursorError::UnknownOrExpiredKey`; a valid MAC under a retained key may then
return typed expiry, rotation, or binding diagnostics. Restart reloads active
and retained verification keys from the read snapshot.

## Summary DAG

LCM summaries are immutable derived nodes with exact source anchors, source horizon, model/configuration route, creation watermark, and sanitization receipt. A summary cannot replace or hide its source.

Publishing a summary atomically commits the node, source edges, content, and anchor manifest. Missing, stale, deleted, redacted, unauthorized, cyclic, or unverifiable sources make the node unavailable for current answers. Corrections publish successor lineage and stale affected descendants; they never rewrite history.

Context assembly may use a summary only when its horizon covers the selected evidence. Exact source text remains retrievable when required by the query or budget permits it. Summary drill-down may follow Plan 13 anchors to GitHub, CI, diagnostic, or Git evidence, but the summary node itself never becomes the durable store for those classes.

## Plan 37 reuse without a parallel kernel

[Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) may reuse
this plan's session context expansion, temporal modes, ranking fusion, and compact
context assembly for branch-aware feedback capsules and advisory proximity context.
That reuse delegates to this sole temporal retrieval kernel through typed
application requests. Plan 37 must not add a parallel LCM engine, summary store, or
second hydration path for GitHub, CI, diagnostic, or Git evidence. Those products
resolve through Plan 13 anchors and their owning stores; Plan 37 binds cycle
results and capsules to those references instead of copying durable evidence into
session payloads.

Read-only GitHub review and CI-failure ingress does not require
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md). The executable-work journey
may optionally compose already-shipped read-only operations through
[Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md); it
never enables a GitHub write path. LCM and summary payloads remain
session-narrative authority only with no write-side GitHub path.

## TaskId-rooted reuse without task authority

In the executable-work journey, an authorized
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) `TaskId`
may select task-linked session, Thread, Turn, message, agent, and tool narrative
through this sole temporal kernel in `current`, `as_of`, `evolution`, or
`forensic` mode. Plan 24 remains task identity and graph authority. GitHub, CI,
diagnostic, Git, code-generation, review, artifact, and runtime evidence
resolves through Plan 13 anchors and its owning stores; no task evidence is
copied into LCM.

The PR8 kernel stays task-agnostic. Executable-work application composition
supplies the authorized selector and reuses PR8's scope, temporal, hydration,
cursor, coverage, and expansion contracts without changing PR8 storage or
sequencing.
A handoff-oriented assembly profile returns coverage and unresolved gaps first,
compact task-linked narrative second, and exact chronology only by authorized
expansion. Summaries accelerate retrieval but cannot replace raw messages or
external anchored evidence.

No PR8 domain record, store port, SQL table, schema-admission receipt, refresh key,
cursor, query request, or application request contains `TaskId`. The
executable-work application translates an authorized `TaskId` into an ordinary
PR8 request without changing PR8 storage, sequencing, authority, or scope.

## Side-effect-free reads and freshness

Search, direct-anchor lookup, LCM describe/expansion/expand-query, hydration,
replay, and continuation use `GlobalDbReadSnapshot`. They never ingest provider
history, create a database/file/key/cursor, repair a store, open a writable
fallback, start refresh, advance source progress, or mutate access metadata.

`crates/tracedecay-domain/src/session.rs` adds `SessionSourceIdV1`,
`SessionSourceFrontierV1`,
`ClosedUtcIntervalV1 { from_inclusive: Option<UtcMicros>,
through_inclusive: Option<UtcMicros> }`,
`ValidCoverageIntervalV1::{Known(ClosedUtcIntervalV1), Unknown}`,
`SessionSourceCoverageIntervalV1 { knowledge: ClosedUtcIntervalV1,
valid: ValidCoverageIntervalV1 }`,
`SessionTemporalCoverageRequestV1 { mode: TemporalModeV1 }`,
`SessionSourceCoverageStateV1::{Fresh, Stale, Partial, Locked, Redacted,
RetentionWithheld, Unavailable}`,
`SessionSourceCoverageV1 { source_id, observed_frontier, committed_frontier,
target_watermark, request, covered_intervals, missing_intervals, state, reason }`,
and
`SessionSourceCoverageReceiptV1`.

Each closed interval requires at least one bound and
`from_inclusive <= through_inclusive` when both exist. Covered and missing
lists sort canonically by knowledge bounds and then valid-time discriminant and
bounds; duplicate, overlapping-on-both-axes, or mergeable-adjacent entries are
invalid. `TemporalModeV1::AsOf { cutoff }` is the sole cutoff field, so wire
values cannot disagree.

The receipt is carried through `SessionRefreshProgressV1`,
`SessionRefreshReceiptV1`, `SessionTemporalExecutionReport`, and
`crates/tracedecay-usecases/src/session/types.rs::SessionTemporalMetadataView`. The view binds
the requested mode/cutoff and the complete sorted source receipts. Aggregate
freshness is derived from source receipts and is never hard-coded to `Fresh`.
Wrong scope is the request-level
`SessionRetrievalOutcome::ScopeUnavailable`, not a source coverage state. Empty
results distinguish no relevant evidence from scope-unavailable and from mixed
stale, partial, retained, locked, redacted, or unavailable source coverage.

Freshness is an explicit daemon operation.
`crates/tracedecay-domain/src/session.rs` owns
`SessionRefreshSourceTargetV1 { source_id, observed_frontier,
target_watermark }` and
`SessionRefreshKeyV1 { store_root_id, session_id, sources,
projector_version, configuration_digest }`. `sources` is nonempty, sorted by
source ID, unique, and included in the canonical key digest.
`crates/tracedecay-store/src/session/refresh.rs` consumes these types in
`SessionRefreshStore`. Fields that alter projection output participate in
equality; query-only mode and grain do not.

`SessionRefreshStore::begin_or_join` joins only identical keys. Different
source sets/frontiers/targets cannot join. The daemon scans each source once,
atomically commits sanitized observations and source progress, resumes the last
committed frontier after restart, and gives every joiner the same terminal
receipt. Cancellation returns that frontier and never rolls it back.

## Result and context contract

Each result includes a stable retrieval anchor, logical occurrence/cluster or
derived identity, Turn/session/thread/source identity, independent knowledge
and valid-time state, safe snippet, evidence/authority class, every
`RetrieverContributionV1`, calibration/diversity decisions, source coverage
receipt, and positional hydration availability.

Pages use stable evidence ordering and the canonical cursor contract above.
Empty results retain per-source coverage and abstention reasons.

Compact context contains only selected Turns, spans/bursts, exact supporting
occurrences, summary lineage when used, conflicts, typed omissions, source
coverage, and continuation anchors. It never dumps a transcript or unrelated
agent activity. Expansion from a span, burst, or summary is lossless to every
authorized occurrence and preserves unavailable members by ordinal.

## PR8 implementation sketch (non-normative)

The paths, type names, schema objects, and adapter locations below record how
PR8's design divided ownership. They are not a request to recreate an obsolete
module tree or database shape. The maintained requirement is the authority
boundary and callable behavior described above. Refactors may move, rename, or
replace these artifacts when direct regressions continue to prove one temporal
kernel, immutable evidence, rank-final hydration, canonical pagination,
truthful source coverage, lossless authorized expansion, and side-effect-free
reads.

### Domain

- `crates/tracedecay-domain/src/session.rs` owns occurrence, bitemporal,
  assertion, copy, Turn/thread, derived evidence, retriever contribution,
  source coverage, summary lineage, refresh key, and wire-validation types.
- `crates/tracedecay-domain/src/research/subjects.rs` and
  `crates/tracedecay-domain/src/research/anchor.rs` own typed derived entities
  and complete `DerivedFrom` anchor lineage.
- `crates/tracedecay-domain/tests/session_contract.rs` owns serialization,
  malformed-manifest, independent-time, and authority validation.

### Store

- `crates/tracedecay-store/src/session/common.rs` owns
  `SessionFrozenWatermarksV1`, expanded to the sorted per-session/per-source
  manifest.
- `projection.rs::SessionTemporalProjectionStore` owns generation writes,
  derived records/members, count/digest receipts, ready/active transitions, and
  immutable replay.
- `retrieval.rs::SessionRetrievalStore` owns frozen compact reads and
  `expand_derived_members(snapshot, id, after_ordinal, limit)`.
- `summary.rs::SessionSummaryStore` owns immutable summary publication and leaf
  eligibility.
- `refresh.rs::SessionRefreshStore` owns begin-or-join, progress, cancellation,
  recovery, and terminal source-coverage receipts.
- `crates/tracedecay-store/tests/session_contract/{projection,retrieval,summary,refresh}.rs`
  own the corresponding port contracts.

### Query and application

- `src/query/temporal/ports.rs` owns `TemporalReadPort`, `TemporalRecord`, and
  the frozen execution manifest.
- `src/query/temporal/candidates.rs` owns bounded candidate planning and
  exact-literal/span/burst channels.
- `src/query/temporal/resolution.rs` owns bitemporal mode, copy, assertion,
  conflict, summary-horizon, and leaf-eligibility resolution.
- `src/query/temporal/ranking.rs` owns calibrated fusion, retained retriever
  contributions, deterministic dedupe/diversity, and rank explanation.
- `src/query/temporal/cursor.rs` owns the authenticated canonical cursor.
- `src/query/temporal/hydration.rs` owns `TemporalHydrationPort`,
  `RankedHydrationResultV1`, `RankedHydrationPayloadV1`, and rank-final
  authorized hydration; unavailable payloads reuse domain `HydrationStateV1`.
- `src/query/temporal/context.rs` owns exact-budget context assembly.
- `src/query/temporal/mod.rs::execute_temporal_kernel` is the sole orchestration
  entry point.
- `crates/tracedecay-usecases/src/session/retrieval.rs::SessionRetrievalService::retrieve`
  owns authorization and canonical request construction.
- `crates/tracedecay-usecases/src/session/ports.rs::SessionTemporalExecutionPort` owns the
  application/kernel boundary.
- `crates/tracedecay-usecases/src/session/refresh.rs::SessionRefreshService` owns explicit
  refresh begin-or-join, status, and cancellation.
- `crates/tracedecay-usecases/src/session/types.rs` owns application request, result,
  freshness, abstention, and coverage views.

### Database, daemon, and final-store admission

- `crates/tracedecay-global-db/src/session_temporal/schema.rs::ensure_session_temporal_schema`
  validates the exact final schema, append-only triggers, derived
  evidence/member tables, source-manifest tables, and receipt columns. Any
  other shape returns typed `ResetRequired` before interpretation.
- `crates/tracedecay-global-db/src/schema_contract/definitions.rs` registers every new table,
  foreign key, trigger, and index; `crates/tracedecay-global-db/src/schema_stages.rs` keeps their
  installation atomic.
- `crates/tracedecay-global-db/src/session_temporal/{projection,rebuild,retrieval,hydration,refresh}.rs`
  implement store/query ports without adding policy.
- `crates/tracedecay-global-db/src/session_temporal/cursor_keys.rs` owns explicit write-side key
  provisioning/rotation, 24-hour cursor expiry, and retained verification-key
  loading; read paths cannot create or rotate keys.
- `crates/tracedecay-global-db/src/session_temporal/operations/publication.rs::publish_immutable_summary`
  owns canonical atomic summary publication.
- `src/daemon/session_temporal_refresh_scheduler.rs` owns durable restart
  recovery and source scanning; it does not own query ranking or hydration.
- `src/mcp/tools/handlers/session/message_search.rs` and
  `src/mcp/tools/handlers/session/lcm_handlers/mod.rs` translate an evidenced
  independently released request protocol to the application service. Missing
  service wiring returns typed unavailable or deferred output and never probes
  an older persisted shape.

The schema-v3 derived projection tables are:

```sql
session_derived_evidence(
  session_id, generation, evidence_kind, evidence_id,
  retrieval_anchor_id, thread_id,
  first_occurrence_id, last_occurrence_id,
  algorithm_version, configuration_digest,
  member_count, member_digest, evidence_json,
  PRIMARY KEY(session_id, generation, evidence_kind, evidence_id)
);

session_derived_evidence_members(
  session_id, generation, evidence_kind, evidence_id,
  ordinal, occurrence_id, member_role,
  PRIMARY KEY(session_id, generation, evidence_kind, evidence_id, ordinal),
  UNIQUE(session_id, generation, evidence_kind, evidence_id, occurrence_id)
);
```

`schema.rs` creates `idx_session_derived_evidence_scope_order`,
`idx_session_derived_evidence_anchor`,
`idx_session_derived_evidence_thread_order`, and
`idx_session_derived_evidence_members_occurrence`, then validates each exact
column and index shape before opening the final store. A prior
`session_messages` or `lcm_*` shape returns `ResetRequired`; no compatibility
projection, reader, conversion, or data import is retained.

## Direct production verification

Acceptance follows callable behavior, not historical test names, suite counts,
module registration, fixture paths, benchmark phases, or command inventories.
Current direct tests and ordinary repository checks must exercise:

- independent knowledge and valid time in all temporal modes, immutable
  occurrence/assertion/copy/derived/summary authority, and deterministic
  one-shot, incremental, and restart rebuilds;
- exact-literal retrieval, stable fusion/provenance, contradiction-first
  dedupe, canonical diversity, rank-final hydration, and lossless authorized
  expansion with positional typed omissions;
- authenticated continuation across every participating source watermark,
  precise rejection of tampering or semantic drift, bounded cursor size, key
  rotation/expiry, and restart;
- the same authorization, deletion, redaction, retention, lock, and privacy
  behavior across search, direct lookup, expansion, hydration, replay, and
  continuation, with no sensitive value entering indexes, summaries, receipts,
  explanations, logs, or dynamic sinks;
- source-aware refresh joining, atomic progress, cancellation, idempotent
  runtime receipts, restart recovery, and truthful mixed freshness;
- exact-final-schema admission, typed `ResetRequired` for every other
  persisted shape, explicit reset/recreation, and no legacy import or
  compatibility projection; and
- side-effect-free public reads and one production temporal kernel, with no
  writable fallback, alternate cursor/hydration path, project-registry fanout,
  or copied external-evidence authority.

Developer resource measurements may report rebuild, replay, ranking, hydration,
and expansion cost from a simple Linux run. They are diagnostic evidence, not a
machine-independent threshold or separate acceptance artifact.

## PR8 behavioral acceptance

- Immutable occurrence, logical-copy, Turn/thread, temporal-assertion,
  evidence-span/burst, ordered-member, and summary-lineage contracts exist with
  complete Plan 13 anchors and independent knowledge/valid time.
- Rebuildable projections and indexes include derived member manifests,
  exact-literal support, source manifests, and receipt digests; deterministic
  rebuild and exact-final-schema/`ResetRequired` tests pass.
- One temporal kernel serves message, Turn, session, thread, agent, span/burst,
  summary, direct-anchor, LCM, and workflow-recovery reads.
- Compact candidates are temporally resolved, fused with full retriever
  provenance, deduplicated/diversified by canonical source/thread/session keys,
  and ranked before payload hydration.
- Authorized late hydration preserves page rank, membership, and cursor under
  payload, availability, and authorization changes.
- The authenticated cursor binds all semantic inputs and every participating
  session/source watermark; every drift test passes.
- Source-specific freshness and explicit daemon refresh survive restart,
  cancellation, concurrency, and mixed source state without fabricating
  empty-complete output.
- Search, direct anchor, describe, expansion, expand-query, hydration, replay,
  and continuation pass the same privacy/authorization matrix and create no
  files, rows, keys, repairs, refreshes, cursor state, or writable connections.
- Raw occurrences, derived member lineage, and summary leaf lineage remain
  losslessly recoverable subject to typed authorization/retention omissions.
- LCM, summary, and derived payloads remain session-narrative projections only;
  GitHub, CI, diagnostic, Git, receipt, task, and `rh_` evidence stays on Plan
  13 anchors and in owning stores.
- PR8 remains task-agnostic and single-root. The scope plan owns cross-project
  composition, Plan 24 owns TaskId composition, and the dashboard journey owns
  its binding; explicit reset/recreation leaves no physical legacy table path.
- Current direct regressions exercise every behavior above through the
  callable current temporal retrieval, refresh, and expansion paths,
  with ordinary repository checks covering supported features. Obsolete
  artifact-name, source-layout, command, or test-count parity is not an
  acceptance criterion.
