# Retired V2 baseline-refresh candidate packet

**Status:** retired. The packet was a temporary review aid and never had
standing authority. No candidate-packet lifecycle remains.

The former document duplicated the audit, assigned historical FM numbers, and
described generated slice bindings, `next-ready`, `BASELINE_FM_BINDINGS`, and
plan-tool validation. All of that machinery is removed. Product work is stated
and tested directly in its owner plan.

## Accepted handoff

- Plan 02 owns periodic exclusive maintenance and reclamation receipts.
- Plans 04 and 16 own idempotent per-shard fan-out and restart catch-up.
- Plan 07 and plan 27 own host hook coverage, safe delivery, and trust state.
- Plan 12 owns bounded daemon shutdown, canonical profile paths, resumable
  cursor migration, and explicit old-cursor disposition.
- Plan 23 owns compression replay identity and `cursor`/`hermes` continuity.
- Plan 26 owns separate observable outcomes and per-event notification counts.
- Plan 14 owns focused fault and regression tests for those behaviors.

Required regression outcomes are concise: a failed project shard catches up
without duplication; a never-resolving shutdown task cannot hold the process
open indefinitely; symlinked or relocated profiles export or return an explicit
skip receipt; cursor/provider migrations resume idempotently; scheduled
maintenance reclaims deferred pages; hook trust ownership is unambiguous; and
notification retries do not multiply either event type.

## Closure

There is no generated candidate, baseline-refresh command, FM routing ledger,
plan parser, progress tracker, or execution gate to maintain. See the named
owner plans for current scope and acceptance.
