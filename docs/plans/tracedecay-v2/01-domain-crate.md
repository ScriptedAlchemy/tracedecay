# V2 domain boundary

## Status / Role

PR5 observation contracts are complete. `tracedecay-domain` is the pure value-and-validation
boundary used by vertical product PRs. It is not a standalone framework roadmap.
Delivery order and global rules live in [the plan index](00-plan-set-index.md)
and [the V2 overview](README.md).

## Outcome

Product slices exchange stable Rust values without leaking database rows,
provider payloads, transport shapes, paths, or runtime handles across ownership
boundaries. A public contract is added only in the same PR as its first product
consumer.

## Owns

- Versioned value types, identifiers, validation, and deterministic encoding.
- Pure research/evidence contracts already landed in PR4.
- Shipped observation, source-position, sanitization-receipt, sensitivity, and
  retention values required by capture and persistence.
- Scope values that distinguish profile-wide user data, project-wide facts and
  sessions, and branch/worktree-scoped code graphs.
- Immutable provenance, coverage, ordering, and watermark values introduced by
  the vertical slice that consumes them.

## Does not own

- Filesystem, database, network, clock, async runtime, locks, queues, or secrets.
- Provider parsing, redaction execution, persistence, projection, querying,
  ranking, policy execution, transport, rendering, or host integration.
- Documentation enforcement, delivery orchestration, source-derived metadata,
  or duplicate transport-local models.
- Speculative schemas, registries, or type families without a shipping consumer.

## Required behavior

- PR4 keeps the crate free of I/O and root-crate dependencies.
- PR5 derives observation identity from stable source evidence, never a row ID,
  absolute path, ambient CWD, or provider display label.
- PR5 permits durable content only after classification and sanitization; every
  durable payload is bound to a receipt covering its digest and disposition.
- PR5 values preserve malformed, partial, duplicate, late, redacted, rejected,
  and unavailable evidence as explicit typed outcomes.
- PR5 source positions and cursors are provider-safe opaque values; numeric and
  content-hash cursors cannot be compared under the wrong ordering rule.
- Each later vertical PR adds the smallest contract it consumes and proves the
  old version remains readable or supplies an explicit migration.
- Provider-exposed reasoning may be represented with visibility and retention;
  hidden reasoning is never inferred or reconstructed.

## Acceptance

- PR4: an architecture test proves `tracedecay-domain` has no I/O, database,
  transport, provider, or root dependency.
- PR5: golden tests prove stable observation identity and canonical encoding.
- PR5: negative tests reject unclassified durable payloads, receipt/digest
  mismatch, invalid source position, and scope ambiguity.
- PR5: serde round trips preserve unknown provider evidence without making it an
  indexed or executable field.
- Every PR changing a public value includes its consuming test in that same PR;
  unused public vocabulary fails review.
