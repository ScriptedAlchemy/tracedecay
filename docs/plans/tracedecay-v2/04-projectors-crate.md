# V2 projection boundary

## Status / Role

PR5 pinned the first production observation-to-view contract. Projection now
participates in each active vertical PR that introduces or replaces a product view. It
is not a standalone framework, registry, or generated-inventory project. See
[the plan index](00-plan-set-index.md) for the owning slices and
[the V2 overview](README.md) for global rules.
Each projection/synchronization slice records lag, throughput, resource, and
no-op baselines for [PR20](33-end-to-end-performance-optimization.md).

## Outcome

Immutable sanitized observations deterministically produce existing product
views. Incremental replay and a rebuild at the same committed frontier produce
the same rows, order, provenance, coverage, and checkpoint.

## Owns

- Pure observation-to-view derivation and stable projector versioning.
- Idempotent output keys, provenance links, coverage, and source watermarks.
- Projector checkpoint semantics and dead-letter disposition required by the
  product view introduced in the same PR.
- Rebuild validation and atomic publication when a view uses generations.
- PR9 current-diagnostic derivation from sanitized, identity-matched clean
  generation evidence.
- Doctor/operations read models introduced by the PR14 product slice.

## Does not own

- Provider discovery, parsing, sanitization, source offsets, or hook ingestion.
- Database connections, transactions, writer leases, or publication mechanics;
  the daemon store adapter implements those contracts.
- Query parsing/ranking, policy execution, application commands, transport,
  rendering, repair execution, scheduling, or task/workflow execution.
- Dirty LSP overlay diagnostics or per-client document state; those remain
  ephemeral daemon session state under
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- A complete projector registry, dependency planner, compatibility metamodel,
  speculative view family, or copied canonical transcript store.

## Required behavior

- PR5 pins one captured observation family and proves its deterministic mapping
  to the existing searchable product row without changing capture truth.
- A projector consumes only sanitized observations and receipt-validated fields;
  it cannot scan or redact content or mint sanitization eligibility.
- Effects and checkpoint commit atomically through the daemon store adapter.
  Failure, cancellation, stale authority, gap, or blocking dead letter leaves
  the checkpoint at the prior committed input.
- Duplicate delivery is a no-op. Late and corrected evidence produces explicit
  provenance or supersession rather than an in-place historical rewrite.
- PR9 projects only sanitized diagnostics whose repository, snapshot,
  generation, file identity, and content digest match the clean code view.
  Dirty-overlay diagnostics bypass durable projection and become eligible only
  after saved content enters the normal capture and generation pipeline with
  the same digest.
- Current and rebuilt diagnostic views honor clearing and supersession evidence;
  they never revive or publish stale, historical, or cross-snapshot findings as
  current.
- Incremental and rebuild execution at the same frontier are byte-stable for
  rows whose representation is ordered; generated views publish only after
  validation and keep the prior validated generation on failure.
- Provider expansion PRs add only the mapping needed for that provider and prove
  parity with its PR5 contract before exposing the view.
- Canonical transcript bodies remain profile-wide. Project views contain scoped
  rows or locators, never copied message authority.
- Project facts and sessions are project-wide. Code projections require the
  exact repository, checkout, worktree, ref, snapshot, and generation and never
  fall back to an active branch.
- PR14 Doctor/operations projections expose real health, lag, corruption,
  recovery, and repair receipts; they do not manufacture findings from source
  code or documentation metadata.

## Acceptance

- PR5: a direct contract test maps the real provider observation to the expected
  existing row with stable identity, provenance, scope, and sanitized content.
- Each provider PR proves duplicate and reordered delivery converge on the same
  output and checkpoint.
- Each view PR proves an injected output failure rolls back effects and
  checkpoint together, then succeeds on replay.
- Each view PR using generations proves rebuild equals incremental at a frozen
  frontier and failed validation leaves the prior generation active.
- PR9 diagnostic tests prove dirty overlays create no durable projection,
  mismatched identities cannot enter current views, and rebuild preserves
  clears and supersession without reviving stale findings.
- Scope tests prove user/project ownership and reject base-checkout fallback for
  branch/worktree code graphs.
- PR14 tests prove Doctor diagnosis remains read-only and repair views reflect
  only authoritative, receipt-bearing operations.
- PR13 parity and restart tests must pass before any superseded V1 projection
  path is removed.
