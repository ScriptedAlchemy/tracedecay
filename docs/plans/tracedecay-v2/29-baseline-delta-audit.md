# Retired V2 baseline delta audit

**Status:** retired historical artifact. It owns no product behavior, release
gate, execution state, generated inventory, or follow-up subsystem.

The former audit compared fixed historical Git endpoints and routed findings
through an evidence packet. Those endpoint counts, commit inventories, worker
transcripts, and audit labels are provenance only. Current source, tests, and
the owning product plans are authoritative.

## Product obligations transferred to owners

| Owner | Remaining direct product obligation |
|---|---|
| [02 Store](02-store-crate.md) | Keep daemon-owned physical writers. Schedule bounded exclusive maintenance independently of upgrades and emit reclamation receipts. |
| [04 Projectors](04-projectors-crate.md) | Make user/project fan-out idempotent and durable; persist per-destination receipts and reconcile a failed shard with the same message identity. |
| [07 Hooks](07-hooks-crate.md) | Preserve deliberate host event asymmetries, define the trust-state owner, and deliver only through supported safe boundaries. |
| [12 Migration](12-root-compatibility-migration.md) | Bound process shutdown even when an outer background await stalls; make default-profile path comparison canonical; specify resumable `user-turn-v2` resweep and old-cursor cleanup. |
| [14 Regression](14-historical-failure-regression-matrix.md) | Keep direct regression tests for every transferred failure mode without recreating an audit ledger or plan validator. |
| [16 Scope](16-cross-project-repository-worktree-scope.md) | Reconcile partial project projections without confusing project-wide session/fact identity with branch-scoped code graphs. |
| [23 Session/LCM](23-session-lcm-temporal-retrieval-and-evaluation.md) | Preserve compression replay identity and make historical `cursor` versus current `hermes` provider eras explicitly migrated or queryable. |
| [26 Observability](26-observability-accounting-and-usage.md) | Distinguish partial projection, skipped export, runtime-drop timeout, outer shutdown timeout, and both notification event types. |
| [27 Host bundles](27-cross-host-agent-plugin-bundles.md) | Pin host-specific hook coverage and trust behavior, including Claude-only `PostToolUseFailure`, without copying host logic. |

Notification tests must cover `turn_completed` and `turn_ingested` separately;
each may emit once per user/project destination and must deduplicate retries.

## Closure

Plans 02/04/07/12/14/16/23/26/27 own implementation and acceptance. This file
does not defer work, refresh a baseline, or authorize a parser, generator,
progress tracker, completion ledger, or rewrite executor. It remains only to
explain where the concrete findings moved.
