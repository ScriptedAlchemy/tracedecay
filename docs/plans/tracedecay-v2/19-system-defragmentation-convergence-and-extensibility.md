# System Convergence and Extensibility

## Status / role

V2 architecture constraints apply throughout delivery. PR19 performs the
complete forward migration and atomic product cutover, then deletes superseded
V1 implementations and temporary convergence machinery. This plan constrains
that production journey; it is not a second product model.

Names of superseded wrappers, ownership inventories, scorecards, ledgers,
route registries, packets, and migration scaffolds are historical evidence,
not prerequisites or features to recreate. Stable published aliases and
persisted formats remain compatibility contracts until their declared
migration or retirement; all other convergence is judged by the direct
behavior and architecture boundaries below.

This cutover inventories only V1 APIs/data proven on `origin/master`, in a
published package/release, or in live persisted storage. `TraceDecay V2` is
the product destination, not evidence for contract-version siblings.
Unreleased source aliases, adapters, and transient DTOs converge in place and
do not acquire migration or deprecation windows from PR sequencing or tests.
Any branch-written schema, store, spool, file, journal, checkpoint, receipt, or
projection stays in the cutover inventory until a separately authorized
registered-store/profile census proves absence.

## User outcome

After upgrading, every supported surface reaches one coherent V2 system.
Existing data and stable APIs continue to mean the same thing, while the daemon
is the only storage authority and canonical application operations own product
behavior. Recovery is bounded and forward-only. V1 and migration-only paths no
longer remain available as hidden fallbacks.

## End-to-end production path

1. Plan 12 preflights, backs up, stages, and verifies every supported released
   V1 data family under the owning daemon's maintenance fence.
2. Production callers are migrated to canonical V2 domain and application
   operations. CLI, MCP, hooks, dashboard, LSP bridge, HTTP, and SDK adapters
   translate requests but do not own storage, policy, query, scheduling,
   diagnostics, or lifecycle behavior.
3. Stable public compatibility aliases with release evidence are bound to those same canonical
   operations and preserve semantic and lifecycle equivalence. Temporary
   aliases identify their consumer, owner, exact deletion condition, and latest
   delivery slice and are removed in PR19 after that consumer migrates.
4. One atomic release cutover publishes the verified V2 store/schema epoch and
   V2 composition root. Before cutover V1 is authoritative; after cutover one
   fenced V2 daemon owns each mutable shard and stale binaries or clients fail
   with an actionable upgrade/reconnect outcome.
5. Failed pre-cutover staging is resumable or discardable. Post-cutover
   rollback restores forward to the exact prior verified V2 epoch from verified
   backup/archive material. There is no reverse cutover, dual write,
   production shadow read, or lazy migration.
6. After the bounded recovery window passes, V1 stores, implementations,
   writable fallbacks, migration-only adapters, dead features and dependencies,
   and their dedicated tests are deleted.

If migration, parity, authority, or recovery evidence is missing or partial,
the operation returns a blocking or `insufficient_evidence` outcome and keeps
the current authority unchanged.

### Retained owners and extension behavior

- Domain modules retain invariants and stable value types; application modules
  retain use-case coordination, policy, authorization, and transactions;
  infrastructure retains stores, providers, runtimes, and operating-system
  effects; adapters retain syntax translation only.
- Storage, configuration, identity, query, diagnostics, lifecycle,
  repair, and durable disposition derivation each have one canonical owner.
  Reads never repair. Skip/collision/refusal dispositions are interpreted in
  that derivation owner rather than patched independently into drain, audit,
  and rebuild callers.
- Extensions use typed revisioned capabilities and declare compatible
  protocol/schema/capability ranges, lifecycle class, canonical operation, and
  unsupported behavior. They cannot bypass policy or daemon authority.
- PR17 workflows remain typed stored definitions whose steps invoke existing
  authorized daemon operations. PR19 does not replace them with workflow
  JavaScript, repository scripts, or another task runtime.
- The daemon gateway plus thin bridge remains the sole analyzer/diagnostic
  lifecycle after Plan 35 parity and rollback acceptance. Canonical registry,
  configuration, store, and query owners remain unchanged.

## Implementation slices

### Migrate callers and state forward

- Move complete feature families to the canonical owner rather than wrapping a
  partial V2 implementation around remaining V1 logic.
- Keep domain invariants and stable values free of transport/database concerns;
  application operations own authorization and transactions; infrastructure
  owns stores, providers, runtimes, and operating-system effects; adapters only
  translate.
- Use Plan 34's transactional API-migration path for source/API changes and
  Plan 12 for stored data. Source migration never substitutes for store
  migration.

### Cut over one authority

- Publish V2 composition and storage together only after real user journeys
  prove semantic equivalence.
- Preserve exactly one writer before, during, and after cutover. Hooks remain
  bounded event clients; reads never perform repair; repair and convergence
  loops have one daemon-owned writer below all callers.
- Preserve evidence-backed compatibility aliases, pagination, streams,
  cancellation, retry and error behavior through the canonical operation
  instead of parallel compatibility implementations. Delete branch-only
  aliases in place after their callers move.

### Recover, converge, and delete

- Exercise bounded pre-cutover resume and post-cutover forward restoration,
  including crash/restart and stale-binary fencing.
- Remove duplicate root, dashboard, host, adapter, analyzer, diagnostic,
  query/render, parser, and storage paths once the canonical journey is
  accepted.
- Remove obsolete external `ast-grep` probing, subprocess outline/rewrite,
  direct writable clients, dead feature flags, unused dependencies, and
  migration-only build/test support.
- Preserve normal/optional/build/development dependency ownership, isolate
  heavy providers/grammars/model runtimes/transports/dashboard generation from
  unrelated focused checks, keep build-script rerun inputs narrow, and align
  integration-test targets with measured product workflows. Plan 33 owns
  retained same-host performance comparisons; PR19 does not invent a crate
  quota or machine-local build policy.

## Replacement and deletion

The surviving system has one owner for storage, configuration, identity,
query, diagnostics, scheduling, and lifecycle. Extensions use typed,
revisioned capabilities and cannot bypass authorization or daemon authority.
A new crate is retained only for a real ownership/runtime boundary or a
measured build-graph benefit; file size, speculative reuse, or package-count
targets do not justify it.

PR19 removes every unreleased temporary wrapper whose consumer has migrated.
Stable public aliases remain only when release evidence makes them actual
compatibility contracts, and they delegate
all availability, errors, authorization, effects, health, paging, streaming,
cancellation, and retries to the canonical operation.

Generated ownership inventories, architecture scorecards, convergence ledgers,
route registries, and declaration-only boundary checks are deleted. Focused
behavior and dependency-direction tests protect the surviving architecture.

## Direct acceptance

- Released V1 fixtures upgrade through Plan 12 and complete representative
  CLI, MCP, hook, dashboard, LSP, HTTP, and SDK journeys through canonical V2
  operations with semantically equivalent values, errors, redaction, and
  effects.
- Fault injection proves atomic cutover, one-writer fencing, crash/restart,
  failed daemon replacement, bounded forward recovery, and refusal of stale
  binaries on Linux and Windows.
- No surface starts its own analyzer lifecycle, opens writable product storage,
  owns diagnostic state, or repairs on read after cutover.
- Stable compatibility aliases execute the same behavior and lifecycle as
  primary names. No unapproved temporary alias or duplicate implementation
  remains.
- Dependency-direction tests prevent domain/application layers from importing
  adapters or concrete stores, and focused build/test workflows do not pull
  unrelated heavy subsystems without measured ownership justification.
- Ordinary aggregate repository checks pass after direct journey and recovery
  tests; no separate acceptance gate is created. PR19 ends with no V1 runtime,
  dual read/write, lazy migration, reverse cutover, writable fallback, skipped
  family, migration TODO, or migration-only path.

## Not in PR19

- New feature semantics, a second workflow runtime, or speculative extension
  framework.
- A crate-count target, generated product model, architecture dashboard,
  execution ledger, schema-only conformance suite, or placeholder baseline.
- Autonomous Git history mutation or restoration of V1 as writer.
