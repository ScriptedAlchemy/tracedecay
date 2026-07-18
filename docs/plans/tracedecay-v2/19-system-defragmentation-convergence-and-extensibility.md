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
- Repository-controlled compilation boundaries, dependency fan-in, and test
  target ownership where they follow product ownership.
- PR19 deletion of obsolete modules, adapters, shims, and compatibility paths.
- Architectural tests that protect real boundaries.

## Does not own

- Feature-specific behavior defined by the other plans.
- A target package count, crate quota, generated inventory, or architecture scorecard.
- A plan parser, progress tracker, executor, or generated view of the repository.
- Workflow JavaScript or a second orchestration runtime.

## Required behavior

1. Daemon authority
   - Before PR16, one local daemon owns product database connections, writes,
     migrations, and recovery. PR16 preserves exactly one fenced daemon
     authority per mutable shard; no client or peer becomes a second writer.
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
   - Compile-time savings justify extraction only when same-host measurements
     show a smaller frequently touched graph after accounting for added crate
     metadata, code generation, and linking.
   - File size, naming preference, or speculative reuse alone does not justify a crate.

4. Canonical operations
   - Storage, configuration, privacy, identity, query, and lifecycle behavior each have one owner.
   - Adapters call those operations rather than reimplementing them.
   - Extensions use typed, revisioned capabilities and cannot reach around
     policy or daemon authority. Each adapter declares the canonical operation,
     compatible capability/protocol/schema revision range, lifecycle class,
     and unsupported behavior; generated schemas or successful compilation do
     not establish semantic or lifecycle conformance.
   - A compatibility alias may translate wire shape only. Availability, errors,
     authorization, effects, health, and cancellation come from its canonical operation.
   - Reads never repair. Status and read projections report stored state;
     convergence and repair loops have exactly one owner below all callers,
     and callers are one-shot. PR7 accumulated three divergent copies of one
     derived-memory convergence policy — scheduler, curation, and a startup
     loop — plus status reads that performed repair writes; that shape is the
     anti-pattern this rule forbids.
   - Durable dispositions are consulted at the canonical derivation, not
     patched around it. When an input has a recorded disposition (skip,
     collision, refusal), the one derivation path returns the typed outcome;
     drain, audit, and rebuild consumers must not each implement coordinated
     pre-checks or substitutions over a disposition-blind core. PR7's
     output-collision handling began as three coordinated special cases and
     converged to one disposition-aware derivation; new dispositions start in
     that shape.

5. Typed workflows
   - PR17 represents dynamic workflows as typed, stored definitions.
   - Workflow steps invoke existing authorized daemon operations.
   - There is no JavaScript workflow SDK, repository workflow script, or parallel task runtime.

6. Convergence and deletion
   - Each replacement identifies its canonical owner and removes the superseded path.
   - Every alias, wrapper, or adapter is classified
     `stable_public_contract` or `temporary`, names its external consumer and
     owner, and for temporary surfaces records the exact deletion gate and
     latest delivery slice. Missing disposition blocks convergence.
   - Temporary adapters have an explicit deletion condition within the delivering PR sequence.
   - PR19 removes all satisfied shims, duplicate paths, dead feature flags, and obsolete dependencies.
   - After [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
     parity and rollback gates pass, PR19 removes legacy root-, dashboard-, and
     host-owned analyzer lifecycle; direct per-language host plugins; duplicate
     adapter or extension tables; direct diagnostic caches or stores; and every
     writable fallback.
   - The surviving diagnostic path is the daemon gateway plus thin bridge,
     canonical registry/configuration, and canonical store/query operations.
   - Delete external `ast-grep` probing, subprocess outline/rewrite, duplicate
     parser acquisitions, surface-local handlers/query/render/database logic,
     and superseded semantic aliases after their bounded compatibility window.

7. Developer feedback topology
   - Each crate owns only the normal, optional, build, and development
     dependencies required by its product boundary. Heavy providers, grammars,
     model runtimes, transports, and dashboard generation remain isolated from
     unrelated focused checks and tests.
   - Integration-test targets align with product ownership and measured focused
     workflows. A name filter is not treated as proof of narrow compilation.
   - Build scripts declare narrow rerun inputs and skip generation or asset work
     when the relevant inputs and features are unchanged or disabled.
   - Optimize boundaries from the PR7+ developer-feedback evidence in Plan 00;
     do not create a crate-count target or machine-specific build policy.

## Acceptance

- Dependency checks prevent domain and application layers from importing adapters or concrete stores.
- CLI, MCP, hooks, UI, and SDKs perform product work only through daemon application operations.
- Concurrent clients cannot become additional database authorities.
- Every surviving crate has a documented ownership or dependency reason beyond file organization.
- PR19 removes superseded implementations, compatibility shims, dead flags, and unused dependencies.
- Every surviving compatibility surface has a stable-public-contract
  disposition; every removed temporary surface has passed its named
  conformance and deletion gate without changing PR ownership or sequencing.
- No client, dashboard, root compatibility path, or host plugin starts
  analyzers, opens writable stores, owns diagnostic state, or bypasses the
  canonical daemon gateway after cutover.
- Every high-fan-in crate, heavy default feature, build script, and oversized
  shared test target has a current ownership reason or same-host evidence for
  retaining it; focused workflows do not compile unrelated heavy subsystems.
- Direct behavior and boundary tests replace generated inventories and architecture scorecards.
- No plan parser, tracker, executor, generated product model, or workflow JavaScript remains.
