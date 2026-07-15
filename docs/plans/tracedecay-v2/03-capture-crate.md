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
