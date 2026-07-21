# V2 capture boundary

## Status / Role

PR5 sanitized Claude capture is complete. This boundary now owns PR6 provider
expansion. It moves
existing product ingestion behind one deterministic privacy boundary; it is not
a crate-first framework project. Shared sequence and ownership rules are in
[the plan index](00-plan-set-index.md) and [the V2 overview](README.md).

## Outcome

One existing provider first, then the remaining supported providers, produces
immutable sanitized observations through the daemon-owned store authority.
Restart, replay, and duplicate delivery preserve every committed record and
never skip a suffix.

## Owns

- Bounded provider discovery, framing, parsing, and normalization.
- Stable source identity, record position, rewrite detection, idempotency key,
  and next-offset derivation.
- The one runtime classification, redaction, rejection, and receipt-producing
  path before durable persistence.
- Provider-specific coverage and malformed/unknown-version outcomes.
- PR6 adapter additions that reuse the PR5 contracts and authoritative sink.

## Does not own

- Database connections, paths, transactions, writer recovery, or fallback
  persistence. Capture calls the daemon-owned store adapter.
- Canonical projection, query/ranking, policy decisions after capture, public
  transport semantics, dashboard views, or Doctor repair execution.
- Hook-side database access or workflow execution. Hooks emit bounded events or
  signals to the daemon.
- Documentation-driven orchestration, generated adapter matrices, or a
  parallel source-of-truth schema.

## Required behavior

- PR5 routed one existing provider from its current parser through
  classification, sanitization, receipt creation, atomic persistence, and replay.
- Raw content remains transient until sanitized. Logs and errors contain only
  safe reason codes, counts, and identifiers.
- Observation identity is stable across restart and independent of scan order,
  database row identity, and absolute checkout path.
- Observation, receipt, and source offset commit atomically. Failure or
  cancellation before commit advances nothing; acknowledgement occurs after it.
- Exact duplicates are idempotent. Conflicting duplicates, malformed input,
  partial records, unknown versions, redaction, and secret rejection are visible
  typed outcomes rather than silent drops.
- Linked worktrees resolve to the canonical project store. Missing, ambiguous,
  stale, or unauthorized project/user authority fails closed without another
  writer.
- PR6 adds each provider through the same sanitizer and sink and retains its
  current ordering, origin, usage, tool, reasoning-visibility, and cursor
  behavior unless the PR records an intentional compatibility change.
- Provider-exposed reasoning follows its explicit retention and search policy;
  capture never infers hidden reasoning.

## Generic external-source convergence

The first external-source admission path ships with
`crates/tracedecay-capture/src/source/{mod,contract,ports,state,admission}.rs`,
`src/capture/source_daemon.rs`, and `tests/external_source_capture.rs`. It
consumes Plan 02's `crates/tracedecay-store/src/source/{mod,records,traits}.rs`
and Plan 27's existing acquisition envelopes, refresh requests, cursors,
scheduling, and concrete adapters; capture defines no second connector or store
module.

```rust
pub trait CanonicalSourceAdmission: Send + Sync {
    async fn admit_envelope(
        &self,
        authority: SinkAdmissionProofV1,
        request: CanonicalPageAdmissionV1,
    ) -> Result<SourceCommitReceiptV1, SourceAdmissionErrorV1>;
}

pub struct CanonicalPageAdmissionV1 {
    pub definition: SourceDefinitionV1,
    pub binding: SourceBindingSnapshotV1,
    pub refresh_receipt_id: SourceRefreshReceiptId,
    pub envelope: SourceRecordEnvelopeV1,
    pub partition_id: SourcePartitionId,
    pub expected_cursor: Option<SourcePartitionCursorV1>,
    pub next_cursor: Option<SourcePartitionCursorV1>,
    pub snapshot_id: Option<SourceSnapshotIdV1>,
    pub coverage: SourceFrontierCoverageV1,
}
```

Incremental admissions require `next_cursor = Some(_)` and compare it to the
expected partition cursor. Whole-root admissions may be cursorless but require
a stable `snapshot_id`; mode-inconsistent requests are rejected before
sanitization or persistence.

An external event is wake-up evidence, never canonical content. Event admission
computes a stable content-free key and returns a receipt that Plan 09 uses to
authorize a Plan 27 `SourceRefreshRequestV1` against the pinned definition,
binding, configuration, and grant:

```rust
pub struct EventAdmissionReceiptV1 {
    pub receipt_id: EventAdmissionReceiptId,
    pub binding_id: SourceBindingId,
    pub refresh_id: SourceRefreshId,
    pub disposition: EventAdmissionDispositionV1,
    pub duplicate_of: Option<EventAdmissionReceiptId>,
}

pub enum EventAdmissionDispositionV1 { Enqueued, Coalesced, Duplicate }
```

The receipt contains no title, body, excerpt, path, URL, native payload, or
provider-rendered content and cannot implement or substitute for a
`SanitizationReceiptV1`, retrieval anchor, observation, or effect receipt. It
triggers canonical refetch through Plan 27; only
`SourceRecordEnvelopeV1` values produced by that canonical read may enter
`CanonicalSourceAdmission`. A duplicate receipt references the original and
reuses its refresh ID without scheduling another refresh. A poison-event test
makes event content disagree with refetched content and proves only refetched
sanitized content can become durable or searchable.

Delivery is at least once. Stable admission keys, `SourceId`,
`NativeObjectId`, `SourceRevisionId`, sanitized digests, and compare-and-set
frontiers provide idempotency; no source or transport path claims exactly-once
delivery. One `(owner, SourceBindingId)` may have at most one active refresh and
one coalesced successor under Plan 27's refresh scheduler. Capture deduplicates
overlapping envelopes and pages; Plan 27 coalesces event/poll acquisition.
Reuse of an event identity with different safe metadata is a typed conflict.
The Plan 01 `Event`, `Poll`, and `Hybrid` classifications map to Plan 27
`EventHint`, polling modes, and event-plus-repair-poll behavior.

The pure state machine in `state.rs` is:

```rust
pub enum SourceAdmissionStateV1 {
    Received,
    Sanitizing,
    Committing,
    Retryable,
    Blocked,
    Complete,
}
```

Legal transitions are `Received -> Sanitizing -> Committing -> Complete`,
retryable admission/commit failure to `Retryable -> Received`, and identity,
authority, privacy, unsupported-revision, cursor-gap, or completeness
violations to `Blocked`. Plan 27 owns `Pending`, lease, fetch, retry, event
coalescing, and polling transitions. Cancellation or failure before the atomic
store commit advances no partition frontier.

`ConnectorContractV1` selects `Event`, `Poll`, or `Hybrid` and declares
`WholeRoot`, `IncrementalRevision`, or
`IncrementalWithWholeRootFallback` as the normalized classification of Plan
27's acquisition contract. Whole-root pages must share one provider snapshot;
capture admits complete-snapshot and absence evidence but Plan 04 alone derives
and publishes absence tombstones. Partial, cancelled, mixed-revision,
unauthorized, or unavailable scans carry incomplete coverage and cannot prove
absence. Incremental partition cursors must be gap-free; object
`SourceRevisionId` values never serve as cursors, omission never means deletion,
duplicate pages are no-ops, and a revision reused with a different digest
blocks that partition without frontier advance. Plan 27 may fall back from
incremental to whole-root only when the pinned contract explicitly permits it.

Canonical changes are typed as `Upsert`, `Correction { predecessor }`, and
`Tombstone { predecessor: Option<_> }`. Corrections and tombstones append
immutable sanitized observations and lineage and never rewrite prior evidence.
Replayable external sources persist bounded operations, receipts, and frontiers
only; they do not persist a raw-content spool. Refresh operation durability
remains in Plan 27's acquisition state/receipts rather than a capture-owned
scheduler table. The existing bounded local
non-replayable host-admission spool remains Plan 27 scope, while
[the remote shared-Brain plan](28-remote-multi-machine-shared-brain.md) owns
offline capture and replay. Neither spool
defines source identity or substitutes its receipt for a source revision.

## Implementation and verification

Dependency order is: existing Plan 27 acquisition contracts and native
fixtures; Plan 01 normalized identities/definitions; Plan 16 owner resolution
and Plan 20 protected binding configuration; existing Plan 13 anchor contracts;
Plan 06 proof contracts; Plan 02 source transaction; the Plan 03 pure admission
transition and sanitizer; Plan 04 projection ports; Plan 09 effect
orchestration; then Plan 23 temporal interpretation. Plan 13 anchors join
retained evidence in the Plan 02 transaction. Plan 27 owns adapters,
network acquisition, event/poll scheduling, refresh retries, packaging,
install/update/repair/uninstall, and host UI. Capture owns none of those
lifecycle or public-surface concerns.

Plan 09 activates a connector only after Plan 06 authorization. Before the
first provider fetch, every page continuation, and canonical admission,
application rechecks source grant ∩ requester grant ∩ resolved owner scope ∩
sink policy, including mandatory local privacy, against pinned definition,
binding, configuration, and sink revisions. Capture receives a non-forgeable
`SinkAdmissionProofV1`; missing or stale authority blocks before network
access or persistence.

The consuming Plan 09 application slice owns the exact typed use cases
`PublishSourceDefinitionV1`, `DryRunSourceBindingChangeV1`,
`ApplySourceBindingChangeV1`,
`AdmitSourceEventV1`, `RequestSourceRefreshV1`, `GetSourceRefreshV1`,
`CancelSourceRefreshV1`, `RebuildSourceProjectionV1`, and
`ValidateSourceGenerationV1`, `PublishSourceGenerationV1`,
`RollbackSourceGenerationV1`, and `RetireSourceGenerationV1`. Plan 20
implements the protected binding change; Plan 27 executes refresh lifecycle;
Plan 03 performs canonical admission; and Plan 04 rebuilds/publishes local
projections. These are internal application contracts, not new
CLI/MCP/HTTP/UI surfaces.

Plan 27's checked-in native bytes under
`tests/fixtures/source_connectors/<source>/` are the sole acquisition fixture
authority. `tests/fixtures/source_connectors/manifest.json` records origin,
native version, path, and SHA-256; Plan 03 adds expected sanitized outputs that
reference the same bytes and hashes. The first source uses
`tests/fixtures/source_connectors/github_review/`, including the existing
event/incremental/whole-root files and the required
`explicit-delete.jsonl`, `corrected-version.jsonl`,
`partial-pagination.jsonl`, and `malformed-record.jsonl`, each paired with an
`*.sanitized.golden.jsonl`. Synthetic lookalike protocol fields are rejected.

TDD order:

1. Fail the event-poison, receipt-content, and raw-log/privacy tests.
2. Fail pure admission-state, duplicate-receipt, storm-coalescing integration,
   and conflict tests.
3. Fail whole-root completeness-evidence and incremental
   object-revision/partition-cursor gap/fallback tests.
4. Fail correction, tombstone, reappearance, and partition-isolation tests.
5. Revoke source grant, requester grant, owner scope, and sink policy
   independently after event admission and before fetch, each continuation,
   and commit.
6. Inject failure at admission, page, observation, receipt, frontier, commit,
   and acknowledgement boundaries; Plan 27 separately tests lease/fetch faults.
7. Prove restart convergence, dropped-event repair by polling, and native
   fixture parity.

Run:

```bash
cargo test -p tracedecay-capture --test event_refetch_contract
cargo test -p tracedecay-capture --test source_admission_state
cargo test --test source_connector_suite
cargo test -p tracedecay-store --test source_contract
cargo test --test external_source_capture
cargo test --test architecture_boundaries capture
cargo check --all-features
cargo nextest run --workspace --all-features --no-fail-fast
```

## Acceptance

- PR5: an end-to-end test proves one real provider yields a sanitized immutable
  observation, matching receipt, searchable product row, and committed offset.
- PR5: replay/restart and duplicate tests prove no duplicate observation and no
  skipped suffix.
- PR5: fault tests before and after each transaction boundary prove complete
  commit or safe retry, with no fallback writer.
- PR5: negative tests cover malformed, partial, conflicting, secret-bearing,
  redacted, stale-owner, ambiguous-worktree, and unavailable-daemon inputs.
- PR6: every added provider has direct golden and incremental/restart tests over
  the shared contracts; adding an adapter creates no database or sanitizer path.
- Linux and Windows-capable focused tests plus workspace format and clippy pass
  for each capture PR.
- Poison event bytes occur nowhere in durable rows, anchors, receipts, logs,
  errors, caches, spools, or projections; the canonical refetch is the only
  content source.
- Duplicate, reordered, concurrent event/poll, crash, and retry delivery
  converges without duplicate observations, skipped revisions, or a false
  exactly-once claim.
- Partial whole-root scans and incremental gaps never infer deletion or advance
  an invalid frontier; Plan 04 alone derives absence tombstones from complete
  evidence, while explicit provider corrections/deletes retain lineage.
- Architecture tests prove no replayable-source content spool, second
  sanitizer, network scheduler, lifecycle/UI implementation, scope resolver,
  or remote outbox.
