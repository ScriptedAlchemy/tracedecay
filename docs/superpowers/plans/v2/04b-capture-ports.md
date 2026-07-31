# Capture Port Boundary Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.

**Goal:** Separate provider capture/sanitization from daemon/store adapters
without weakening one-writer, cursor, replay, or privacy contracts.

## Historical file and interface inventory

- Modify `crates/tracedecay-application` capture use cases and ports.
- Migrate provider families under `src/sessions/**`, `src/agents/**`,
  `src/hooks/**`, and daemon ingest wiring.
- Keep SQL/filesystem/provider adapters in root until PR19.

Public ports:

```rust
pub trait CaptureSink: Send + Sync {
    async fn append(&self, batch: SanitizedCaptureBatch)
        -> Result<CaptureReceipt, CaptureError>;
}

pub trait CaptureCursorStore: Send + Sync {
    async fn load(&self, source: SourceIdentity) -> Result<SourceCursor, CaptureError>;
}

pub trait CaptureAdmission: Send + Sync {
    async fn authorize(&self, request: CaptureRequest)
        -> Result<CaptureGrant, CaptureError>;
}
```

Only sanitized, identity-bound batches cross `CaptureSink`. Cursor identity
includes profile, project/store, provider/source, and generation.

## Historical task checklist

- [ ] Add compile failures for application-to-root/provider adapter imports.
- [ ] Add sanitization-before-write and no-raw-payload boundary tests.
- [ ] Introduce sink/cursor/admission ports and root adapters.
- [ ] Migrate one provider family per commit, preserving replay receipts.
- [ ] Verify every supported host/provider and one-writer admission.

## Product outcome contributed

Provider capture and sanitization became separable from daemon/store adapters
without weakening one-writer, cursor, replay, identity, or privacy behavior.
Current direct behavior and acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

No format cutover occurs until a provider's old and new paths produce identical
receipts. Revert provider slices independently; never dual-write. Measure
provider-private and root adapter edits. Delete direct provider-to-store writes
only after all production callers, crash/replay tests, and host audits use the
ports.
