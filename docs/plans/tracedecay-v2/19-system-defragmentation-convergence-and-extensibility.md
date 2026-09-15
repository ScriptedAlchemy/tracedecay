# System convergence and extensibility

## Status / role

Plan 19 keeps V2 on one canonical product path. It moves complete caller
families to the domain, application, infrastructure, and adapter owners that
serve real journeys, then deletes duplicate internal routes and scaffolding.

Fresh V2 profiles admit the final product shape. This plan does not introduce a
stored-data reader, converter, backfill, dual write, profile census, or staged
database cutover for branch-era work.

## User outcome

Every supported surface reaches the same behavior, errors, authorization,
lifecycle, and availability semantics through one daemon-owned authority. A
missing or invalid local product state is reported truthfully with reset or
recreation guidance; it is never silently reinterpreted by a fallback path.

## Convergence rules

- Domain modules own invariants and stable values; application modules own
  authorization and use-case coordination; infrastructure owns storage,
  providers, runtimes, and operating-system effects; adapters only translate
  syntax.
- Storage, configuration, identity, query, diagnostics, scheduling, repair,
  and durable disposition each have one owner. Reads do not repair.
- Unreleased source-only aliases, internal DTOs, and adapters change in place
  and disappear with their last real caller.
- An independently released public protocol may retain a documented
  compatibility delegate when release evidence identifies the external journey.
  The delegate calls the canonical operation and cannot fork policy, storage,
  lifecycle, or error behavior.
- Extensions declare typed capabilities, supported revisions, canonical
  operations, and unavailable behavior. They cannot bypass policy or daemon
  authority.

## Delivery slices

1. Trace each complete user journey to its canonical application operation and
   remove parallel dashboard, host, bridge, or runtime implementations.
2. Move remaining direct storage, policy, query, scheduling, diagnostics, and
   lifecycle work behind that operation; retain one writer and typed failures.
3. Delete dead wrappers, flags, dependencies, declaration-only ports, and
   source-only test scaffolds after their final production caller moves.
4. Keep only release-evidenced public delegates, with direct behavior tests for
   their canonical route.

## Direct acceptance

- CLI, MCP, HTTP, dashboard, hooks, LSP, and supported hosts exercise the same
  canonical behavior for representative success, denial, unavailability,
  cancellation, and restart journeys.
- No surface opens writable product storage, owns a second scheduler or
  diagnostic lifecycle, fabricates readiness, or repairs on read.
- Direct tests and ordinary repository checks cover the surviving boundaries;
  inventories, scorecards, route ledgers, and declaration-only gates are not
  acceptance substitutes.
- Public delegates with release evidence preserve their documented protocol
  behavior while source-only callers have no retained alias.

## Not in Plan 19

- A second workflow runtime, speculative extension framework, generated product
  model, or package-count target.
- Branch-scoped fact stores, archive-merge flows, or data movement justified
  only by development history.
- A compatibility layer for unreleased source shapes or an internal persistence
  format merely because it existed on a branch.
