# V2 tool catalog crate

## Status / Role

- Status: pending for PR11.
- PR11 implements the minimal runtime catalog with application and policy consumers.
- PR12 binds the catalog to CLI, MCP, HTTP, dashboard, SDK, and discovery surfaces.
- tracedecay-tool-catalog describes callable product capabilities. It does not discover them by parsing source code.

## Outcome

Every public surface resolves stable capability IDs to the same application use cases, scope rules, effects, privacy requirements, availability, and output semantics without duplicating business logic.

## Owns

- Stable CapabilityId, UseCaseId, and BindingId values.
- Small immutable definitions for capability identity, user-facing description, input/output schema references, effect class, scope requirements, privacy class, availability, deprecation, and surface binding.
- Explicit surface profiles, including bounded MCP profiles and their capability ceilings.
- Immutable catalog snapshot assembly from reviewed contributions registered beside implemented application use cases.
- Pure validation and lookup by stable ID, surface, profile, and availability.

## Does not own

- A source, Clap, Axum, dashboard, hook, Markdown, plan, or workflow parser.
- catalog-gen, inventory JSON, generated architecture views, frozen tool counts, checked-in generated SDK/UI/plugin trees, or CI that reconstructs the product from source text.
- A universal operation registry containing speculative future features.
- Capability execution, authorization, persistence, network I/O, rendering, host probing, installation, or daemon routing.
- A generic invoke-anything tool or compatibility aliases for retired names.
- Task graph, plan editor, workflow executor, or generated agent orchestration operations.

## Required behavior

- **PR11 — definitions:** create compact immutable catalog records and stable IDs for application use cases implemented through PR11. Every entry points to a real typed application handler.
- **PR11 — canonical operations:** structural search, source outline, source
  rewrite, exact/symbol edit, temporal retrieval, configuration, health, and
  every other tool bind stable typed application operations. A surface name or
  alias has zero business logic.
- **PR11 — configuration boundary:** code/config-file inspection is a scoped
  source operation; product settings use the typed configuration authority.
  Similar presentation does not merge their authorization or effects.
- **PR11 — contributions:** register catalog records beside their owning application feature, then assemble one immutable snapshot at composition. No central file duplicates every request/response definition.
- **PR11 — validation:** reject duplicate IDs/bindings, missing handlers, incompatible schema references, invalid scope/effect/privacy combinations, profile overflow, and dependency cycles.
- **PR11 — policy:** expose read-only capability metadata to policy routing. Availability and effect metadata inform a decision but never execute it.
- **PR11 — daemon:** bind each executable capability to the single tracedecayd/application authority. Catalog consumers never open a database or bypass application authorization.
- **PR11 — profiles:** define explicit capability sets and hard ceilings for default, compact, administrative, and host-limited surfaces. Absence is explicit, not a hidden fallback.
- **PR11 — compatibility:** retain a current public name only when a direct compatibility test requires it. Retired names are absent and return typed discovery guidance.
- **PR12 — bindings:** map CLI commands, MCP tools, HTTP operations, dashboard actions, and SDK methods to the same CapabilityId and typed application handler.
- **PR12 — schemas:** surface adapters use reviewed typed schemas or schema references from the owning contract. The catalog does not generate domain types from prose or source parsing.
- **PR12 — discovery:** return bounded capability metadata filtered by surface, profile, availability, scope, and authorization. Never expose secrets, config bodies, private paths, or unavailable administrative details.
- **PR12 — output:** all surfaces consume the same typed application result before rendering. Markdown is the human/agent default where appropriate; structured JSON remains explicit.
- **PR12 — drift:** direct tests enumerate compiled bindings and assert each references a valid catalog entry and handler. This is runtime contract validation, not source extraction.
- **PR13 — hooks:** hook adapters use cataloged host capabilities only through application/daemon responses; hooks do not resolve or execute catalog entries locally.

## Acceptance

- PR11 unit tests cover stable ID serialization, immutable snapshots, duplicate/conflict rejection, profile ceilings, explicit absence, deprecation, availability, and deterministic lookup.
- PR11 integration tests prove every catalog entry resolves to one real application handler with matching scope, effect, privacy, and schema contracts.
- Policy tests cover routing among available entries, missing capability, denied scope, stale availability, and no silent substitution.
- PR12 parity tests invoke representative read, write, administrative, streaming, and long-running use cases through each supported surface and compare typed results before rendering.
- Discovery tests prove compact profiles stay bounded and administrative/private capabilities are filtered correctly.
- Compatibility tests cover only currently supported names and typed guidance for retired names; no frozen total-count assertion is allowed.
- Architecture tests reject source parsers, generators, generated inventories, plan/workflow dependencies, execution logic, storage, transport implementations, and UI code from tracedecay-tool-catalog.
- Repository checks verify no public adapter has a handler-local query, policy, persistence, or authorization path that bypasses the cataloged application use case.

## Refactoring workflow boundary

The catalog owns discovery and typed definitions for refactoring capabilities,
not a second refactoring engine. Read-only `tracedecay_rename_preview`, existing
symbol/string edit primitives, callers/reference discovery, diagnostics, and
future apply operations remain independently callable base tools. Composed
refactoring workflow bundles reference those canonical tools instead of copying
handlers or schemas.

[Workspace refactoring and API migration](34-workspace-refactoring-and-api-migration.md)
owns the behavior and acceptance contract for apply-grade previews, atomic symbol
rename, and semantic API migration. In particular:

- pure symbol rename and compatibility-aware API promotion are separate operations;
- apply tools consume immutable preview/plan identifiers and digests and fail closed on stale evidence;
- catalog capability metadata is granular by language and symbol/site kind;
- unsupported or not-yet-shipped apply operations are never advertised as callable;
- human-readable and JSON results render one typed changed/unchanged/skipped/blocked manifest; and
- neutral adoption evals must prove that agents preview before apply and choose
  semantic migration rather than rename when compatibility or coordinated
  definition changes are required.
