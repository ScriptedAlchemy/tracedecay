# V2 tool catalog crate

## Status / Role

- Status: pending for PR11.
- PR11 implements the minimal runtime catalog with application and policy consumers.
- PR12 binds the catalog to CLI, MCP, HTTP, LSP, and discovery surfaces.
  PR14 first ships dashboard binding, dashboard actions, and dashboard parity
  over the same CapabilityIds and application handlers. PR18 adds SDK bindings
  only when the official SDK methods ship.
- tracedecay-tool-catalog describes callable product capabilities. It does not discover them by parsing source code.

## Outcome

Every public surface resolves stable capability IDs to the same application use cases, scope rules, effects, privacy requirements, availability, and output semantics without duplicating business logic.

## Owns

- Stable CapabilityId, UseCaseId, and BindingId values.
- Small immutable definitions for capability identity, user-facing description, input/output schema references, effect class, scope requirements, privacy class, availability, deprecation, and surface binding.
- Explicit surface profiles, including bounded MCP profiles and their capability ceilings.
- Typed standard-LSP bindings from navigation and diagnostic methods to the
  existing code and diagnostic capabilities and application handlers defined
  for [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Immutable catalog snapshot assembly from reviewed contributions registered beside implemented application use cases.
- Pure validation and lookup by stable ID, surface, profile, and availability.

## Does not own

- A source, Clap, Axum, dashboard, hook, Markdown, plan, or workflow parser.
- catalog-gen, inventory JSON, generated architecture views, frozen tool counts, checked-in generated SDK/UI/plugin trees, or CI that reconstructs the product from source text.
- A universal operation registry containing speculative future features.
- Capability execution, authorization, persistence, network I/O, rendering, host probing, installation, or daemon routing.
- LSP lifecycle, JSON-RPC framing, document synchronization notifications,
  connection state, or other protocol mechanics.
- A generic invoke-anything tool or compatibility aliases for retired names.
- An arbitrary JSON-RPC proxy capability.
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
- **PR12 — bindings:** map CLI commands, MCP tools, HTTP operations, and LSP
  methods to the same CapabilityId and typed application handler where the
  protocol exposes a callable product operation. Dashboard binding, dashboard
  actions, and dashboard parity remain owned by PR14; PR12 does not ship
  dashboard adapters.
- **PR14 — dashboard binding:** map dashboard actions to the same CapabilityId
  and typed application handler as CLI, MCP, HTTP, and LSP. Dashboard parity
  tests submit equivalent typed requests through dashboard and non-dashboard
  adapters and compare semantic results before rendering.
- **PR12 — LSP bindings:** map each supported standard navigation or diagnostic
  method to an existing typed code or diagnostic capability and handler.
  Lifecycle, framing, and document notifications remain protocol mechanics,
  not callable catalog capabilities. `prepareRename` and `rename` bind only to
  read-only candidate/preview UseCaseIds owned by
  [34](34-workspace-refactoring-and-api-migration.md); they never bind directly
  to `tracedecay_rename_symbol`, API-migration apply, another write-effect
  entry, `workspace/applyEdit`, or opaque server commands. No separate
  `lsp_*` capability is cataloged for them, and no binding may apply an edit
  on their behalf. General LSP `textDocument/codeAction` is deferred: it does
  not ship in PR12 and cannot be cataloged until a separate owner defines a
  typed candidate-consumption operation, policy classification, canonical
  preview/`EditTransaction` route, and acceptance fixtures.
- **PR12 — LSP extensions:** require every vendor extension to have an explicit
  typed catalog entry, bounded schema, policy classification, and tested
  handler. Never expose arbitrary method or payload forwarding.
- **PR12 — schemas:** surface adapters use reviewed typed schemas or schema references from the owning contract. The catalog does not generate domain types from prose or source parsing.
- **PR12 — discovery:** return bounded capability metadata filtered by surface, profile, availability, scope, and authorization. Never expose secrets, config bodies, private paths, or unavailable administrative details.
- **PR12 — output:** all surfaces consume the same typed application result before rendering. Markdown is the human/agent default where appropriate; structured JSON remains explicit.
- **PR12 — drift:** direct tests enumerate compiled bindings and assert each references a valid catalog entry and handler. This is runtime contract validation, not source extraction.
- **PR13 — hooks:** hook adapters use cataloged host capabilities only through application/daemon responses; hooks do not resolve or execute catalog entries locally.
- **PR18 — SDK bindings:** add Rust, TypeScript, and Python SDK BindingIds only
  with the shipped typed methods and conformance fixtures. PR12 may describe
  future SDK availability as unavailable protocol metadata but cannot advertise
  an unimplemented SDK method.

## Acceptance

- PR11 unit tests cover stable ID serialization, immutable snapshots, duplicate/conflict rejection, profile ceilings, explicit absence, deprecation, availability, and deterministic lookup.
- PR11 integration tests prove every catalog entry resolves to one real application handler with matching scope, effect, privacy, and schema contracts.
- Policy tests cover routing among available entries, missing capability, denied scope, stale availability, and no silent substitution.
- PR12 parity tests invoke representative read, write, administrative, streaming, and long-running use cases through CLI, MCP, HTTP, and LSP adapters and compare typed results before rendering.
- PR14 contract tests add dashboard binding, dashboard actions, and dashboard parity on the same typed requests and CapabilityIds.
- PR18 SDK parity tests require every advertised SDK binding to resolve to the
  shipped typed method and canonical application handler.
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
