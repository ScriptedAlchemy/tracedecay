# ADR-005: Mandatory Sanitization, Key Domains, and Retention

## Status
Accepted for V2 Phase 0.

## Context
Optional or sink-local regex scrubbing leaves gaps between capture, persistence, indexing, prompts, output, exports, fixtures, and backups.

## Decision
All source content passes one versioned parse-before-scan sanitizer before ordinary persistence or agent exposure. Domain taint states are `Unclassified`, `Classified`, `Sanitized`, and sink-specific eligible wrappers; only eligible values reach a sink. Sanitization receipts record detector/policy versions, coverage, transformations, source-safe fingerprint, and descendant invalidation. Secret plaintext never reaches general logs, indexes, stores, URLs, telemetry, fixtures, exports, or errors.

Privacy/key-domain blobs bind privacy domain, encryption-key epoch, and retention class. Sanitized eligible blobs use authenticated encryption at rest where required; protected raw retention is disabled by default and, when explicitly enabled, uses a separate key authority/quarantine, elevated audited access, no ordinary retrieval/indexing, and a short fixed horizon.

Defaults are exact: ordinary sanitized observations and canonical evidence are retained until a versioned owner policy says otherwise; user deletion uses strict `ingested_at < cutoff`, preserves minimal provenance/tombstone/hold evidence, and is resumable. Protected raw quarantine defaults off; when enabled its maximum default horizon is 24 hours. Export/download artifacts default to 24 hours, response handles to 15 minutes, browser protected caches to profile lock/sign-out or server expiry, whichever comes first. Legal/user holds override deletion eligibility but never authorize a new sink. Key rotation creates a new epoch; cross-epoch dedupe is forbidden.

## Rejected alternatives
- Best-effort output scrubbing or independent per-feature detectors.
- Raw source hashes for secret-bearing records.
- Global content-addressing across privacy/key/retention domains.
- Retention anchored on optional occurred time or immediate hard deletion without receipts.

## Compatibility, rollback, and removal gates
Existing detectors become fixtures or plugins behind the canonical boundary. Differential secret scans, synthetic canaries, sink inventory, rescan/descendant invalidation, restore, key-loss, and deletion-hold tests must pass before old paths retire. Rollback never reintroduces unsanitized writes; protected material remains isolated.