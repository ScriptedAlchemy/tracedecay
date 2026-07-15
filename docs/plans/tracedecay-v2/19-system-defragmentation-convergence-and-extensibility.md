# System Convergence and Extensibility Charter

## Status / Role

- V2 architecture charter applied throughout delivery.
- PR19 completes convergence, deletes superseded paths, and removes temporary compatibility code.
- The charter constrains implementation; it is not a second model of the product.

## Outcome

TraceDecay is one coherent daemon-owned system with clear domain, application, infrastructure, and
adapter boundaries. Features compose through typed operations instead of duplicating storage,
policy, lifecycle, or transport logic.

## Owns

- Dependency direction and component ownership rules.
- The criteria for admitting a new crate or external extension point.
- Convergence of duplicate implementations onto canonical operations.
- PR19 deletion of obsolete modules, adapters, shims, and compatibility paths.
- Architectural tests that protect real boundaries.

## Does not own

- Feature-specific behavior defined by the other plans.
- A target package count, crate quota, generated inventory, or architecture scorecard.
- A plan parser, progress tracker, executor, or generated view of the repository.
- Workflow JavaScript or a second orchestration runtime.

## Required behavior

1. Daemon authority
   - One daemon process owns product database connections, writes, migrations, and recovery.
   - CLI, MCP, hooks, UI, SDKs, and other clients call daemon operations.
   - Hooks send bounded events or signals; they do not implement synchronization or storage policy.

2. Layered ownership
   - Domain modules define invariants and stable value types without transport or database concerns.
   - Application modules coordinate use cases, policy, authorization, and transactions.
   - Infrastructure modules implement storage, providers, runtimes, and operating-system effects.
   - Adapters translate CLI, MCP, HTTP, hooks, and UI requests without owning business rules.

3. Modules first
   - New components begin as modules in the crate that owns their lifecycle.
   - A new crate is admitted only when it creates a real ownership boundary, enforces useful
     dependency direction, supports independent reuse, or isolates a materially different runtime.
   - File size, naming preference, or speculative reuse alone does not justify a crate.

4. Canonical operations
   - Storage, configuration, privacy, identity, query, and lifecycle behavior each have one owner.
   - Adapters call those operations rather than reimplementing them.
   - Extensions use typed capabilities and cannot reach around policy or daemon authority.

5. Typed workflows
   - PR17 represents dynamic workflows as typed, stored definitions.
   - Workflow steps invoke existing authorized daemon operations.
   - There is no JavaScript workflow SDK, repository workflow script, or parallel task runtime.

6. Convergence and deletion
   - Each replacement identifies its canonical owner and removes the superseded path.
   - Temporary adapters have an explicit deletion condition within the delivering PR sequence.
   - PR19 removes all satisfied shims, duplicate paths, dead feature flags, and obsolete dependencies.

## Acceptance

- Dependency checks prevent domain and application layers from importing adapters or concrete stores.
- CLI, MCP, hooks, UI, and SDKs perform product work only through daemon application operations.
- Concurrent clients cannot become additional database authorities.
- Every surviving crate has a documented ownership or dependency reason beyond file organization.
- PR19 removes superseded implementations, compatibility shims, dead flags, and unused dependencies.
- Direct behavior and boundary tests replace generated inventories and architecture scorecards.
- No plan parser, tracker, executor, generated product model, or workflow JavaScript remains.
