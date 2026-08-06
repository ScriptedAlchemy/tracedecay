# TraceDecay V2 Root and Fresh-Store Reset

## Status / role

Normative PR19 plan. Every TraceDecay database, store, spool, file, journal,
checkpoint, receipt, and projection admits only its exact final V2 shape. Any
other shape returns typed `ResetRequired` and requires an explicit reset or
recreation before use.

No V1-to-V2 or earlier-V2 persisted-data reader, conversion, backfill, dual
write, shadow read, census, staging, or recovery path exists, including for
data written by an older installed binary. Only an actually independently
released public wire/API protocol may have separately evidenced compatibility;
source-only aliases, internal adapters, DTO helpers, and branch-local wire
revisions change in place.

**Memory model correction (2026-08-03).** Facts are project-wide. Branch
retirement neither moves nor merges memory facts. It has no special persistence
workflow: non-final memory state returns `ResetRequired`; explicit reset or
recreation is the only transition.

**Root lifecycle correction (2026-07-26).** Direct project init, project open,
read-only open, and branch open acquire the exact profile's owned exclusive
maintenance scope and delegate to registered production authorities.

**Localized integrity repair.** A repair is admissible only for a corrupt,
deterministically rebuildable derivative within an otherwise exact final shape.
Whole-store or authoritative-data corruption remains fail-closed.

Earlier fixture names, family inventories, packet layouts, and transition
scaffolding are historical evidence only. Do not recreate them as product
requirements or runtime machinery.

## Future package-boundary seam

Plan 12 does not prescribe crate-breakup sequencing, source moves, package
counts, worktrees, commits, or delivery gates. Plans 05, 19, 25, and 33 own
future query, code-index, convergence, or build-performance boundaries.

A package boundary is retained only when a direct same-host developer journey
improves and production callers preserve public contracts, generated schemas,
packaging, feature behavior, runtime authority, and normal CI. Source scans,
line/file counts, dependency-shape tables, and moved-module layouts are
diagnostic observations, not acceptance.

## User outcome

An exact-final V2 store opens through one daemon authority. Any other persisted
shape is refused before interpretation with `ResetRequired`; the operator
explicitly resets or recreates that target to obtain a clean final store. No
older data is read or carried forward.

## End-to-end production path

1. The owning daemon takes the exact store's maintenance fence and validates
   every admitted database, store, spool, file, journal, checkpoint, receipt,
   and projection against the final shape.
2. An exact-final shape proceeds through the one-writer daemon route. A
   non-final, partial, unknown, or corrupt authoritative shape returns typed
   `ResetRequired` before any read, write, replay, or projection.
3. An operator-selected reset or recreation makes a clean final store. Bytes
   may be preserved for inspection, never as conversion input.
4. The new final store publishes through the canonical daemon; CLI, MCP, hook,
   API, LSP, dashboard, and SDK clients do not open authority storage directly.
5. A public wire/API compatibility façade is retained only when an actual
   independent release proves its contract. It delegates to the canonical
   operation and owns no storage or lifecycle logic.

Before PR16, one local daemon owns the live store. With remote shared Brain,
exactly one fenced daemon owns each mutable shard. Reset/recreation never
creates another writer.

## Implementation slices

### Admit only the final shape

- Centralize final-shape validation at every persisted-state open boundary.
- Return `ResetRequired` for every mismatched, partial, unversioned, or older
  shape before decoding or mutation.
- Keep deterministic derivative repair bounded to an otherwise valid final
  store; retain typed failure for authoritative corruption.

### Reset or recreate explicitly

- Make reset/recreation an explicit operator action scoped to the exact target.
- Create only the final schema and projections after reset/recreation.
- Keep the single-writer fence across reset/recreation and reconnect supported
  clients through the daemon.

### Remove superseded paths

- Delete storage transition readers, converters, backfills, dual writes,
  shadow reads, staging state, checkpoints, and transition-only dependencies.
- Delete source-only aliases after their named internal callers move. Retain a
  public protocol façade only with actual independent-release evidence.

## Direct acceptance

- Exact-final fixtures for each persisted authority admit and serve through the
  canonical daemon route.
- Every non-final, older, partial, unversioned, or foreign fixture returns
  `ResetRequired` before read, write, replay, or projection.
- Explicit reset/recreation of a refused target yields one clean final store
  and one writer; it does not consume the old bytes.
- Tests prove no storage reader, conversion, backfill, dual write, shadow read,
  census, or recovery path remains.
- Any retained public protocol compatibility test cites its independently
  released contract and proves the façade preserves canonical authorization,
  errors, redaction, effects, pagination, streaming, cancellation, and retry
  behavior without opening storage.

## Not in PR19

- Persisted-data conversion, rollback, or compatibility retention.
- Memory special handling.
- Autonomous Git history mutation or a second compatibility implementation.
- A transition dashboard, execution ledger, schema-only conformance suite, or
  placeholder acceptance baseline.
