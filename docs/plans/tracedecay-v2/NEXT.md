# Next delivery: production store boundary

The next change starts real product implementation.

## Outcome

Create the first production `tracedecay-store` boundary and route one end-to-end runtime slice through it.

## Scope

- Define the store API around the existing V2 domain contracts.
- Integrate sole-daemon database ownership after PR #473 lands.
- Move one real session/event persistence and recovery path behind the store API.
- Keep the root binary operational throughout the migration; no parallel local-write fallback.
- Add direct concurrency, restart, and recovery tests for the migrated path.

## Done when

- Production TraceDecay calls the new store boundary.
- The migrated path has one database authority and no duplicate implementation.
- Direct behavior tests pass on Linux and Windows.
- No inventory generator, generated architecture view, plan parser, or workflow executor is introduced.
