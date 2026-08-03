# V2 domain boundary

## Status / Role

PR5 **session-observation** contracts are complete. External-source delivery is
split (status corrected again 2026-07-26): the host-observation specialization
is live. Hook V2 host admission constructs the retained external-source
definition, binding, authorization, refresh, envelope, frontier, object, and
evidence values and commits them through `SourceCaptureApplicationV1` to the
canonical store. The earlier blanket characterization of this contract as
having no production capture/authorization composition, adapter, or conversion
was wrong for that path. Broader provider acquisition and canonical-refetch
composition remains dormant and is still a retained future seam rather than a
delivered PR5/PR6 capability or PR8–PR14 work to rebuild.

`tracedecay-domain` is the pure value-and-validation boundary used by vertical
product PRs. It is not a standalone framework roadmap.
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
- Each later vertical PR adds the smallest final contract it consumes.
  Pure source-only/internal request helpers change in place. Only an actually
  independently released public wire/API revision may retain protocol
  compatibility. Every persisted domain value accepts only the exact final V2
  shape; any other store, spool, file, journal, checkpoint, receipt, or
  projection returns typed `ResetRequired` and requires explicit reset or
  recreation. There is no storage reader, migration, backfill, dual write, or
  census path; a version suffix or test fixture alone is not release evidence.
- Provider-exposed reasoning may be represented with visibility and retention;
  hidden reasoning is never inferred or reconstructed.

## External-source semantic contract

The canonical domain owner preserves the external-source distinctions consumed
by Plans 02, 03, 04, 09, 16, 20, 23, and 27. It does not prescribe a file
layout or require an unused connector framework.

- Source, binding, partition, partition-cursor, snapshot, native-object, and
  object-revision identities remain opaque and canonically encoded. Native
  object identity is a privacy-domain-bound digest inside one immutable
  binding, never a raw URL, path, repository name, provider key, or credential.
  Object revision and partition cursor remain separate, and object revisions
  have no invented total order.
- A provider-neutral definition pins a nonzero revision, canonical digest,
  capture mode (`Event`, `Poll`, or `Hybrid`), refetch strategy (whole root,
  incremental revision, or explicitly supported incremental-with-whole-root
  fallback), deletion semantics, and bounded partitioning. It is derived by one
  validated conversion from Plan 27's acquisition contract, and its canonical
  digest (computed without the digest field) pins that acquisition contract. It
  does not redefine envelopes, refreshes, cursors, scheduling, or host
  registration.
- The definition contains no owner, endpoint, executable, credential, mutable
  path, scheduler, lifecycle, or UI state. Validation rejects digest mismatch,
  zero partition limits, mode mismatch, complete-snapshot absence without a
  whole-root contract, and fallback not supported by the pinned acquisition
  contract; mismatch is typed failure, never best-effort downgrade.
- Bindings separately carry exact typed `ProjectId` or `UserProfileId`
  authority, source and definition revision, binding revision/digest, native
  root, and privacy domain. Owner, source, native root, and privacy domain are
  immutable across revisions. CWD, checkout paths, labels, collection
  membership, and native identifiers never create or widen authority.
- Binding identity is deterministically derived from source, exact typed owner,
  privacy domain, and the privacy-bound canonical root locator before native
  objects are admitted. Identical provider keys in different projects,
  profiles, or privacy domains cannot collapse.
- Partition frontiers retain cursor, snapshot, continuation, and
  `Complete | Partial | Unknown` coverage. The aggregate digest is
  domain-separated over sorted canonical partition/frontier encodings,
  including coverage; coverage-only changes alter the digest. Aggregate
  coverage is complete only when every active partition is complete. The digest
  is snapshot identity, not a scalar cursor or cross-partition ordering claim.
- Successor, correction, and tombstone lineage cannot cross owner, source,
  binding, privacy domain, or native object. Replay creates neither duplicate
  revisions nor duplicate edges, and lineage remains acyclic.
- Immutable sanitized observations, receipts, anchors, and projections are
  authoritative only for what TraceDecay observed at a committed frontier; the
  external system remains authoritative for current external content.
  `Live`, `AuthoritativeDeleted`, `Partial`, and
  `TemporarilyUnavailable` are content states. `PolicyExcluded` and
  `Unauthorized` are current access states and never rewrite a frontier.
  Explicit provider deletion, or absence in a complete snapshot whose contract
  declares absence semantics, is the only proof of authoritative deletion.
  Partial/unknown coverage, exclusion, access loss, and temporary failure
  cannot prove deletion or a clean empty result.
- Plan 09 composes access and content deterministically: non-disclosing
  exclusion or unauthorized status takes precedence; otherwise exact content
  status passes through, while both axes remain available for audit and replay.

Historical source type names and module/test paths recorded by earlier versions
of this plan are evidence of these distinctions, not current declaration or
scaffold requirements. An audit must locate the current domain owner and verify
the callable behavior and regressions below before calling any old name missing.

## Delivery and regression evidence

The owning product path preserves this dependency direction: domain identities,
definition/binding validation, frontiers, and lineage feed Plan 02 persistence,
Plan 03 capture, and Plan 04 projection. Plan 13 anchors join the first
retained-evidence transaction; Plan 09 owns authorized bind/refresh use cases,
Plan 16 scope resolution, Plan 20 binding configuration mutation, Plan 23
temporal interpretation, and Plan 27 acquisition, scheduling, packaging, and
lifecycle.

The final source-definition and binding path admits current canonical
observations only. Any persisted predecessor source definition, binding,
cursor, frontier, journal, checkpoint, or receipt returns typed
`ResetRequired` and requires explicit reset or recreation; it is never mapped,
read, backfilled, or converted. Unreleased source-only/internal helpers and
branch-local V2 wire-visible revisions finalize in place. An independently
released public wire/API revision may retain a separate evidence-backed
protocol compatibility surface.

Direct regression evidence must prove canonical encoding and unknown-field
handling; digest tamper, raw-identifier, ambiguous-scope, and invalid-capability
rejection; stable binding and project/profile non-collapse; revision/cursor
separation; partition-order-independent aggregate digests; binding-revision
compare-and-set; partial-coverage non-deletion; acyclic
correction/tombstone lineage; and replay idempotency. Provider evidence comes
from checked-in native fixtures replayed through the consuming capture path;
hand-authored lookalike protocol fields are not acceptance evidence.

## Acceptance

- Direct architecture regression proves the domain boundary has no I/O,
  database, transport, provider, settings, credential, lifecycle, UI, or root
  dependency.
- Direct observation regressions prove stable identity and canonical encoding;
  reject unclassified durable payloads, receipt/digest mismatch, invalid source
  positions, and scope ambiguity; and preserve unknown provider evidence
  without making it indexed or executable.
- Every public-value change ships with its consuming behavior test; unused
  vocabulary is not a milestone.
- Native-fixture regressions prove byte-stable encoding and aggregate digests,
  definition/binding separation, exact project/profile authority, no raw native
  identifier or secret, partial-frontier non-deletion, and acyclic lineage.
