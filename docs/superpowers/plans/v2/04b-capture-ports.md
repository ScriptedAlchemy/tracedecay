# Capture Port Boundary Plan

**Goal:** Separate provider capture/sanitization from daemon/store adapters
without weakening one-writer, cursor, replay, or privacy contracts.

## Files and interfaces

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

## Tasks and tests

- [ ] Add compile failures for application-to-root/provider adapter imports.
- [ ] Add sanitization-before-write and no-raw-payload boundary tests.
- [ ] Introduce sink/cursor/admission ports and root adapters.
- [ ] Migrate one provider family per commit, preserving replay receipts.
- [ ] Verify every supported host/provider and one-writer admission.

Direct tests ingest the same provider fixture through ports and production
adapters, then compare durable capture/projection/cursor receipts.

Negative tests cover wrong profile/project/store, cursor aliasing, duplicate
replay, crash between append/cursor, cancellation, privacy rejection,
unavailable admission, and concurrent writer loss.

## Migration, rollback, measurement, deletion

No format cutover occurs until a provider's old and new paths produce identical
receipts. Revert provider slices independently; never dual-write. Measure
provider-private and root adapter edits. Delete direct provider-to-store writes
only after all production callers, crash/replay tests, and host audits use the
ports.
