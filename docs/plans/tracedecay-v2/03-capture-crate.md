# V2 capture boundary

## Status / Role

PR5 sanitized Claude capture and PR6 expansion through the shared
session-observation admission/sanitizer/store path are complete for Claude,
Codex, Cursor, Hermes, Kiro, Cline, Roo Code, and Kilo. External-source
convergence is split (status corrected again 2026-07-26): the host-observation
specialization is live. The MCP Hook V2 entry reaches host admission, which
passes every persisted observation receipt through
`RuntimeExternalSourceStore::capture_host_observation`; that path authorizes
and invokes `SourceCaptureApplicationV1`, then commits through the retained
external-source reducer and SQLite adapter. The earlier claim that
`SourceCaptureApplicationV1` had no production caller, adapter, or migration
was wrong for this specialization. Broader acquisition, scheduled refresh, and
canonical-refetch adapters remain dormant and are not certified as delivered
PR5/PR6 behavior or PR8–PR14 work to rebuild.

This boundary records the deterministic privacy and admission behavior retained
by current product ingestion; it is not a crate-first framework project. Shared
sequence and ownership rules are in
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

## External-source convergence behavior

The current capture owner consumes Plan 02 persistence and Plan 27 acquisition
envelopes, refreshes, cursors, scheduling, and concrete adapters. It defines no
second connector, store, scheduler, or source authority. Earlier module, trait,
DTO, state, use-case, test, and fixture names are implementation evidence, not
scaffold or declaration requirements.

- Canonical page admission carries pinned definition/binding authority, the
  owning refresh receipt, provider envelope, exact partition, expected and next
  cursor or stable whole-root snapshot, and coverage. Incremental admission
  requires and compare-and-sets the next partition cursor. Whole-root admission
  may be cursorless but requires one stable provider snapshot. Mode mismatch is
  rejected before sanitization or persistence.
- An external event is content-free wake-up evidence, never canonical content.
  Event admission derives a stable content-free key and returns an
  enqueued/coalesced/duplicate receipt tied to the binding and refresh. The
  receipt contains no title, body, excerpt, path, URL, native payload, or
  provider-rendered content and cannot substitute for a sanitization receipt,
  retrieval anchor, observation, or effect receipt. It authorizes canonical
  refetch through Plan 27; only the refetched, sanitized provider envelope may
  become durable or searchable. A duplicate refers to the original and reuses
  its refresh without scheduling another.
- Delivery is at least once. Stable admission/source/native-object/revision
  identity, sanitized digests, and frontier compare-and-set provide
  idempotency; no source or transport path claims exactly-once delivery.
  Plan 27 permits at most one active refresh and one coalesced successor per
  owner/binding. Capture deduplicates overlapping envelopes/pages, while Plan
  27 coalesces acquisition. Reusing an event identity with different safe
  metadata is a typed conflict.
- The pure admission lifecycle is received, sanitizing, committing, and
  complete; retryable admission/commit failure returns to received; identity,
  authority, privacy, unsupported-revision, cursor-gap, or completeness
  violations block. Plan 27 owns pending, lease, fetch, retry, event
  coalescing, and polling. Cancellation or failure before atomic store commit
  advances no partition frontier.
- Event, poll, and hybrid modes and whole-root, incremental, or explicitly
  supported fallback strategies remain the normalized Plan 27 acquisition
  classification. Whole-root pages share one provider snapshot. Capture admits
  complete-snapshot and absence evidence, but Plan 04 alone derives absence
  tombstones. Partial, cancelled, mixed-revision, unauthorized, or unavailable
  scans cannot prove absence. Incremental cursors are gap-free; object revision
  never serves as cursor; omission never means deletion; duplicate pages are
  no-ops; and a reused revision with a different digest blocks only that
  partition without frontier advance. Incremental-to-whole-root fallback
  requires explicit support in the pinned contract.
- Upserts, corrections, and tombstones are distinct canonical changes.
  Corrections and tombstones append immutable sanitized observations and
  lineage and never rewrite prior evidence.
- Replayable external sources persist bounded operations, receipts, and
  frontiers, never a raw-content spool. Plan 27 owns refresh durability and the
  bounded local non-replayable host-admission spool; Plan 28 owns remote offline
  capture/replay. Neither spool defines source identity or substitutes its
  receipt for a source revision.

## Ownership and regression evidence

The retained dependency direction is Plan 27 acquisition and native evidence;
Plan 01 identities/definitions; Plan 16 owner resolution; Plan 20 protected
binding configuration; Plan 13 anchors; Plan 06 proof; Plan 02 atomic store
commit; Plan 03 admission and sanitizer; Plan 04 projection; Plan 09
orchestration; then Plan 23 temporal interpretation. Plan 27 owns adapters,
network acquisition, scheduling/retries, packaging, lifecycle, and host UI;
capture owns none of those concerns.

Plan 09 activates acquisition only after Plan 06 authorization. Before the
first provider fetch, every continuation, and canonical admission, application
rechecks source grant, requester grant, resolved owner scope, sink policy, and
mandatory local privacy against pinned definition, binding, configuration, and
sink revisions. Capture receives non-forgeable admission authority; missing or
stale authority blocks before network access or persistence.

The application owner retains callable behavior to publish definitions;
dry-run/apply protected binding changes; admit events; request, inspect, and
cancel refreshes; rebuild projections; and validate, publish, roll back, and
retire generations. Plan 20 implements protected binding changes, Plan 27 the
refresh lifecycle, Plan 03 canonical admission, and Plan 04 projection
rebuild/publication. These are internal application contracts, not automatic
new CLI/MCP/HTTP/UI surfaces.

Checked-in native Plan 27 bytes with recorded origin, native version, and digest
are the sole acquisition evidence. Sanitized expectations reference those same
bytes; synthetic lookalike protocol fields are rejected. Direct regressions
must cover poison-event/refetch authority, content-free receipts and logs,
admission lifecycle, duplicate/storm coalescing and conflict, whole-root
completeness, incremental gaps/fallback, correction/tombstone/reappearance,
partition isolation, independent revocation of every authority before fetch,
continuation, and commit, failure at every admission-to-acknowledgement
boundary, restart convergence, dropped-event repair by polling, and native
fixture parity.

An audit must map these requirements to current callable owners and direct
regressions before reporting a gap. A renamed or deleted historical mechanism
does not require reconstruction.

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
- Focused Linux and Windows-capable regressions preserve the same capture
  behavior.
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
