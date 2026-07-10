# TraceDecay V2 Tool Catalog Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Create one versioned, generated capability catalog that makes every TraceDecay use case discoverable and semantically consistent across MCP, CLI, HTTP, dashboard, skills, hooks, policy routing, documentation, and compatibility migration.

**Architecture:** tracedecay-tool-catalog is a pure metadata/compiler crate. Checked-in typed use-case definitions reference domain schemas and declare ownership, effects, scope, freshness, privacy, cost, evidence, compatibility, and transport bindings; generators emit immutable manifests and adapter metadata, while audit extractors compare every live/legacy surface against the catalog. The crate never executes a use case, performs discovery I/O at runtime, or becomes a second application layer.

**Tech Stack:** Rust 2024; serde/serde_json; schemars/jsonschema; semver; blake3; thiserror; clap Command introspection in a build/audit binary only; OpenAPI schema fragments; TypeScript/JSON generation; insta/proptest; V2 domain contracts.

---

## 1. Contract Lock

This plan owns master-plan PR 22A. It lands before tracedecay-policy PRs 23A–23G so policy bundles can pin a catalog digest, and before hook PR 24F so host descriptors bind to stable capabilities.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) contributes initiative/plan/task/query/control/lifecycle, executor registration/protocol, scheduler, packet, and task-view capability families. This catalog owns their audience/effect/scope/grant/privacy/egress/idempotency/output metadata and generated bindings; `all/*` and generic tool grants never enable task mutations implicitly.

- Stable use-case identity is transport-independent. search, tracedecay tool search, a future HTTP route, a dashboard command, and a skill route can be bindings of one use case rather than five implementations.
- tracedecay-tool-catalog describes use cases and bindings. tracedecay-application implements/orchestrates them. Adapters invoke application ports; the catalog invokes nothing.
- tracedecay-domain owns canonical IDs, schemas, scope, sensitivity, evidence, watermarks, query/cursor, and command semantics.
- tracedecay-policy consumes one immutable ToolCatalogSnapshot and returns routing/evaluation decisions. It cannot patch the catalog during evaluation.
- Generated artifacts are deterministic from definition/schema/legacy-inventory inputs. Their digest participates in policy/hint/replay manifests.
- An unavailable, pending, deprecated, incompatible, stale, redacted, credential-gated, or live-refresh-required capability remains discoverable with a reason; it does not vanish.
- Surface parity means shared semantic request/response/effect/error contracts, not identical presentation. Markdown, JSON, CLI text, and UI may render differently from one typed result.
- [`20-configuration-control-plane.md`](20-configuration-control-plane.md) owns typed configuration descriptors and effective-value semantics. This catalog generates config bindings and proves full surface coverage; it does not define settings, precedence, or defaults. The config-metadata pipeline runs in exactly one direction: plan 20's registry generator emits `generated/config-registry-v1.json` (typed descriptors plus schema fragments) as an input manifest to this catalog build, the snapshot pins its `ConfigRegistryDigest`, and this catalog is the sole emitter of config CLI/MCP/OpenAPI/SDK/dashboard-form/docs surface metadata; plan 21 renders only from these catalog artifacts.
- [`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md) owns the exhaustive current CLI/MCP/output audit and generated binding/presentation parity contract. This crate emits that metadata; it cannot keep a second format/scope/dispatch/allowlist inventory.
- [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) consumes catalog-declared scout/model/tool eligibility, read-only effect class, egress/privacy, budgets, and delivery bindings; no daemon allowlist is legal.
- [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md) replaces legacy message/LCM binding semantics with one generated temporal search/context/replay/evaluation family while retaining old names only as bounded compatibility rows.
- PR #410's direct_user/subagent/tool_result filters and parent-representative dedupe are semantic query capabilities, not presentation-only toggles.
- Git output distinguishes directly_changed, structurally_impacted, candidate_test, and context_only. Transitive/file-level graph fanout can never be labeled direct modification.

## 2. Goals

- Inventory every current MCP tool, top-level and recursive CLI command, HTTP method/route, dashboard plugin/action, managed/bundled skill, hook event/effect, config mutation, background operation, and compatibility alias.
- Assign stable CapabilityId, UseCaseId, IntentId, BindingId, semantic version, owner, lifecycle, and replacement to each.
- Require domain `ScopeSelectorV2` on every scoped capability/binding; catalog metadata can constrain allowed scope kinds but cannot invent a transport-specific/current-project selector.
- Generate MCP schemas/descriptions/categories, CLI command metadata/help cross-references, HTTP/OpenAPI operation metadata, dashboard command/panel manifests, skill/tool references, hook routing facts, documentation tables, and TypeScript catalog types from one definition set.
- Make read/mutate, manual-versus-autonomous execution, side effects, idempotency, dry-run/preview, confirmation, automatic recovery/compensation, streaming/pagination, cost, latency, freshness, security, privacy, and audit behavior explicit. Curation item effects are autonomous and have no approval/apply binding.
- Give policy compact, versioned task-to-capability facts without shipping the entire catalog in every hint.
- Catalog current agent-presence/work-claim publish, heartbeat, nearby-query, overlap acknowledgement/handoff, coordination analytics, and Coordination Lab capabilities with advisory/privacy/TTL/trigger semantics.
- Route Git intent to branch_list, branch_search, branch_diff, pr_context, changelog, commit_context, sessions_for, and workflows with exact local/live/joined truth requirements.
- Reconcile local semantic Git state and live GitHub/delivery state by ref/merge-base/head/changed-file universe/fetched-at/index watermark.
- Detect catalog drift in CI whenever a surface is added, removed, renamed, or semantically changed without a catalog/version/parity disposition.
- Preserve V1 behavior as differential/import evidence until each cutover, but publish no old runtime tool names, aliases, response-handle quirks, or stale client schemas afterward. Current capability metadata is authoritative.
- Make missed capability, fallback, user correction, unavailable capability, and useful silence observable outcomes.

## 3. Non-Goals

- No use-case execution, application orchestration, storage/query/policy logic, provider API call, Git/GitHub call, filesystem scan at runtime, dashboard rendering, MCP transport, CLI parser, or Axum router.
- No dynamic plugin marketplace or remote catalog service in the first V2 default.
- No guarantee that every transport exposes every capability. Absence requires an explicit binding disposition and rationale.
- No prose-only routing rules. Prose docs are generated from typed metadata.
- No arbitrary user-authored executable catalog entries. Managed skills may reference registered capabilities but cannot create hidden commands.
- No silent alias reuse after incompatible semantic change. Breaking behavior gets a new use-case major version or replacement ID.
- No conflation of tool invocation with capability success, hint delivery with use, or a skill file's presence with adoption.

### 3.1 Convergence boundary

The catalog is the sole capability/use-case/binding metadata authority in [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md) and the contract-generation input for the official API/SDK plan [`17`](17-official-public-api-and-sdks.md). It references domain/Plan [`18`](18-secret-detection-redaction-and-private-data-safety.md) schemas but owns no runtime scope/privacy behavior.

| Boundary | Contract |
|---|---|
| Enters | Static reviewed definitions, domain/application schema refs, frozen live/legacy surface inventories, lifecycle/compatibility dispositions, and build metadata. |
| Exits | Immutable catalog snapshot/digest, lookup/availability/route facts, generated CLI/MCP/HTTP/OpenAPI/SDK/dashboard/hook/skill/docs metadata, and drift/parity reports. |
| Upstream owners | Domain owns values; application owns use-case behavior/errors; query/policy/hooks own execution semantics; Plan 18 owns privacy eligibility. |
| Downstream owners | Policy routes over pinned facts; application/adapters invoke generated bindings; hooks use descriptors; API/SDK/docs/UI consume generated artifacts. |
| Extension seam | Add one versioned capability/use case plus schema/effect/scope/privacy/cost/evidence owner, bindings/dispositions, fixtures, and generated outputs; never add a surface command/tool first. |
| Scale/concurrency | Pure bounded lookup over immutable CAS-published snapshots; readers pin one digest while generators/auditors run offline. |
| Migration/retirement | V1 inventories/aliases are historical mappings. After binding parity and current-client cutover, old names remain replay provenance only and are absent from live generation/dispatch. |

Catalog errors cover invalid definitions, generation/drift, unknown IDs, incompatible snapshots, and unavailable bindings. They are not application/public business errors. Runtime content cannot enter `CatalogText`; Plan 18 receipts/eligibility appear only as referenced schema and availability requirements.

## 4. Current and Future-Master Inputs

The initial inventory is a timestamped compatibility snapshot, not an eternal count. The canonical counts are 104 source MCP tool definitions at `origin/master` `9f7a1108`, 103 installed at `tracedecay 0.0.47` (which lacks source-defined `move_symbol`; `ast_grep_rewrite` is host-conditional), and 102 at the older frozen compatibility inventory captured on 2026-07-09 from the then-installed binary; the root CLI exposed the commands in Section 5. Live stores and branches continued changing during planning; every generated manifest records binary version, commit, profile, fetched/index watermarks, timestamp, and source digest.

Refreshed implementation inputs:

- The inspected base `99ad19bc` contains merged PR #405 legacy identity-store adoption and #412 daemon/update drain safety. Catalog definitions use adopted identities once and declare lifecycle drain/checkpoint/service-state prerequisites for update/doctor/daemon mutations.
- PR #407 user-profile Hermes consolidation: Hermes skill/memory/automation capabilities belong to the normal active user profile. Removed Hermes bridges/config/inventory are migration aliases, not V2 extension points.
- Merged PR #410 copied-subagent prompt collapse adds direct_user, subagent, tool_result, and parent-representative filters consistently while retaining every sanitized native row and explicit coverage.
- Merged PR #411 foreign-skill ownership makes doctor/removal/update share one ownership predicate; catalog remediation metadata distinguishes actionable-by-this-installation, manual-user-only, and no-action.
- Merged PR #414 adds the `move_symbol` edit capability; regenerate the tool/CLI/API inventory and require owner/schema/scope/effect/idempotency/preview/error bindings rather than treating the old 102-name count as current.
- Merged release PRs #413/#416 and merged #407/#415/#417/#419/#420/#422/#423/#424 contribute current profile, release, identity, edit, routing, catalog-generation, retrieval, and analytics inputs. Open #418 is a refresh input; its live state must be re-read immediately before PR 22A rather than frozen here. PR #409 remains closed historical inventory only.

The implementation lead refreshes master/open PRs and regenerates all legacy inventories before PR 22A. A changed count is expected; an unexplained capability is not.

## 5. Complete V1 Surface Inventory Baseline

Plan 21 §§3–4 own the exhaustive current CLI/MCP audit and are the arbiter whenever inventories disagree; this section is the frozen fixture snapshot that catalog-gen consumes, and it must stay consistent with plan 21's tables rather than becoming a second drifting audit.

### 5.1 MCP/tool surface: 104 source names

The PR 22A fixture locks all 104 source definitions below; 103 are installed at `tracedecay 0.0.47` (which lacks `move_symbol`), and the older frozen inventory listed 102, omitting both `ast_grep_search` and `move_symbol`. Each must map to exactly one use case/version and a lifecycle disposition. Category is presentation metadata, not identity.

| Current category | Current names |
|---|---|
| always-loaded (7) | search, grep, context, callers, status, active_project, storage_status |
| analysis (17) | circular, complexity, constructors, coupling, dead_code, distribution, doc_coverage, field_sites, god_class, hotspots, inheritance_depth, largest, module_api, rank, recursion, unsafe_patterns, unused_imports |
| edit (7) | ast_grep_rewrite, insert_at, insert_at_symbol, move_symbol, multi_str_replace, replace_symbol, str_replace |
| git & history (8) | affected, branch_diff, branch_list, branch_search, changelog, commit_context, diff_context, pr_context |
| graph (14) | by_qualified_name, call_chain, callees, callers_for, derives, file_dependents, find_exact_symbol, impact, implementations, impls, rename_preview, signature, similar, type_hierarchy |
| health (8) | dependency_depth, dsm, gini, health, redundancy, runtime, test_map, test_risk |
| info (35) | analytics, ast_grep_search, automation_run_artifact_view, body, config, dashboard, files, hermes_skill_bridge, lcm_compress, lcm_describe, lcm_doctor, lcm_expand, lcm_expand_query, lcm_grep, lcm_load_session, lcm_preflight, lcm_session_boundary, lcm_status, message_search, node, outline, port_order, port_status, project_context, project_list, project_search, read, retrieve, sessions_for, signature_search, simplify_scan, skill_list, skill_view, todos, workflows |
| memory & session (5) | fact_feedback, fact_store, memory_status, session_end, session_start |
| workflow (3) | diagnose, diagnostics, run_affected_tests |

The inventory generator additionally records parameter schema, required/default/enum/range, description, renderer formats, response-handle behavior, project-selector support, availability, mutation/effect, and dispatch target. A name-only match is insufficient.

### 5.2 CLI surface

Current root commands:

init, sync, status, tool, lsp, install, reinstall, update-plugin, uninstall, dashboard, serve, daemon, upgrade, update, channel, current-counter, reset-counter, disable-upload-counter, enable-upload-counter, gitignore, doctor, cost, bench, gain, monitor, sessions, analytics, projects, branch, memory, automation, migrate, wipe, list, help.

Known recursive paths at the planning snapshot:

- daemon: run, install-service, uninstall-service, restart, status;
- sessions: ingest, search, git-backfill, unfinished;
- analytics: diagnostics, sync;
- projects: list, search, context;
- branch: list, add, remove, removeall, gc, autotrack;
- memory: status, curate;
- automation config: get, explain, enable, disable, set;
- automation run: memory-curation, session-reflection, skill-writing;
- automation runs: list, view, artifact;
- automation skills: list, view, draft, update, approve, disable, archive, restore, install;
- automation facts: list, view, apply, reject;
- branch autotrack: status, enable, disable;
- migrate: plan, export, apply, verify, reconstruct, registry-gc, rollback, cleanup-sources.

PR 22A must recurse clap::CommandFactory through every subcommand and alias, including hidden/deprecated commands, flags, env bindings, defaults, conflicts, validators, mutation/dry-run behavior, JSON support, and help links. The list above is a human audit anchor; the generated recursive fixture is authoritative.

The tool CLI binding tracedecay tool <name> is recorded separately from native CLI commands because it has MCP-argument parity, --args/--dry-run/--json/--project behavior, response handles, and a different compatibility contract.

### 5.3 HTTP and dashboard API surface

Root/shell:

- GET /, GET /shell/{file}, GET /dashboard-plugins/{plugin}/dist/{file};
- GET /api/dashboard/plugins;
- GET /api/projects, GET /api/projects/{project_id}, ANY /api/projects/{project_id}/{tail};
- ANY /api/capabilities, /api/plugins/{tail}, /api/automation/{tail}, /api/settings, /api/settings/{tail}.

Project-routed memory and curation:

- GET /api/capabilities;
- GET /api/plugins/holographic and trailing-slash alias;
- GET /api/plugins/holographic/status;
- GET /api/plugins/holographic/fact/{fact_id};
- GET /api/plugins/holographic/fact/{fact_id}/trust-history;
- GET /api/plugins/holographic/projection;
- GET /api/plugins/holographic/similarity;
- GET /api/plugins/holographic/curation/status, /activity, /runs;
- GET /api/plugins/holographic/fact-proposals;
- POST /api/plugins/holographic/fact-proposals/{proposal_id}/apply and /reject;
- GET/PATCH/DELETE /api/plugins/holographic/curation/config;
- POST /api/plugins/holographic/curate/apply;
- GET /api/plugins/holographic/oplog.

Automation and managed skills:

- GET/POST /api/automation/skills; POST /api/automation/skills/draft;
- GET/PATCH /api/automation/skills/{id};
- POST /api/automation/skills/{id}/approve, /discard-update, /disable, /archive, /restore;
- GET /api/automation/fact-proposals and /{id}; POST /{id}/apply and /reject;
- POST /api/automation/run/memory-curator, /session-reflection, /skill-writing;
- GET/POST /api/automation/jobs; GET/PATCH/DELETE /api/automation/jobs/{id}; POST /{id}/run;
- GET /api/automation/scheduler/status; POST /pause and /resume;
- GET /api/automation/outcomes;
- GET /api/automation/runs/{run_id}/artifacts and /artifacts/{kind}.

The approval/apply/reject/draft/install routes above are V1 inventory only. V2 current bindings replace them with curation status/history/decisions/outcomes, autonomy configuration, pause/resume/run-now, pin/protect/exclude, and feedback. Candidate fact/memory/managed-skill effects are internal autonomous application effects with policy/config/version/validation/outcome/recovery receipts and are never generated as CLI/MCP/HTTP/dashboard item commands.

LCM:

- GET /api/plugins/hermes-lcm/overview, /search, /session/{session_id}, /node/{node_id}, /timeline, /compression, /payloads/health;
- GET/POST /api/plugins/hermes-lcm/payloads/gc.

Graph:

- GET /api/plugins/graph/overview, /search, /node/{node_id}, /node/{node_id}/neighbors, /subgraph, /path.

Analytics, diagnostics, savings, and settings:

- GET /api/plugins/analytics/overview, /hints, /usage, /diagnostics, /underused;
- GET/PATCH /api/plugins/code-diagnostics; POST /refresh and /refresh/{language};
- GET /api/plugins/savings/overview, /ledger, /sessions, /models, /pricing;
- GET /api/settings; PATCH /api/settings/project and /api/settings/user.

Each method is a distinct binding when effects differ. ANY gateways are routing aliases, not unconstrained semantic operations. The audit must expand their resolved target/method set.

### 5.4 Dashboard product/actions

Current registered panels:

- holographic: Holographic Memory explorer/curation;
- hermes-lcm: LCM overview/search/session/node/timeline/compression/payload health/GC;
- graph: code graph overview/search/node/neighbors/subgraph/path;
- savings: overview/ledger/sessions/models/pricing;
- code-diagnostics: overview/settings/refresh all/refresh language;
- settings: project/user/environment/storage/automation configuration.

Automation/skills/fact proposals and analytics APIs exist even when not represented as equal top-level plugins. Catalog disposition must say panel, embedded action, API-only, command-palette-only, legacy-only, or missing parity. Every button/menu/keyboard command is generated or audited by data-testid/action ID and maps to a UseCaseId.

### 5.5 Managed skills

The active profile snapshot contained ten managed skills:

agent-hook-hint-quality-review, agent-hook-latency-profiling, agent-host-diagnostics, agent-tool-event-visibility-investigation, code-slop-cleanup, isolated-worktree-task-flow, mcp-tool-output-rendering-design, skill-writer-evidence-validation, tracedecay-code-context-first, tracedecay-tool-fallbacks.

The catalog records skill package ID/version/checksum/state/targets, referenced intents/use cases/tools, required prerequisites, read/mutate boundary, and provenance. Skill content remains in the skill lifecycle store; the tool catalog stores references/digests, not instructions. Bundled development skills, provider-installed skills, disabled/archived skills, and staged updates also receive explicit inventory state.

### 5.6 Hook and provider surface

Current host entry points to inventory:

- Codex: session_start, user_prompt_submit, subagent_start, post_tool_use, post_compact;
- Claude Code: pre_tool_use allow/deny, session_start, subagent_start, post_tool_use, prompt_submit, stop;
- Cursor: before_submit_prompt, subagent_start, post_tool_use, session_start, session_end, stop, pre_compact, after_file_edit, after_shell, workspace_open;
- Kiro: pre_tool_use, prompt_submit, post_tool_use;
- MCP/daemon hook events: FileEdit, Shell, WorkspaceOpen, SessionStart, IncrementalSync;
- shared effects: capture, inject context/hint, allow/deny, reset/accounting marker, transcript catch-up, LCM lifecycle, file/project sync, branch/worktree tracking, analytics/outcome evidence.

Tool-kind bindings include Codex function_call/function_call_output/custom_tool_call/custom_tool_call_output/local_shell_call/tool_search_call/web_search_call; Claude tool_use/tool_result and parent tool-use IDs; Cursor Agent/Composer invocation/result/edit/plan; automation backend traces; unknown future kinds with opaque schema/coverage.

### 5.7 Configuration and operational mutations

Inventory also includes install/reinstall/update/uninstall integration changes; daemon/service lifecycle; branch tracking/GC; init/sync/wipe; storage/profile migration/repair/rollback/cleanup; counter reset/upload preference; gitignore policy; memory curate/fact feedback/store mutations; automation config/jobs/scheduler/runs/skills/facts; LCM compression/boundary/repair/GC; dashboard settings/diagnostic refresh; edit tools; and response-handle retrieval.

Every mutation declares manual/autonomous execution mode, preview/confirmation when applicable, idempotency, audit event, effect owner, recovery/compensation, and capability availability. Destructive wipe/delete/GC never inherit a generic read binding. Curation candidates declare `autonomous` and therefore cannot generate preview/approve/apply/reject/rollback item bindings.

## 6. Exact File and Module Tree

~~~text
crates/tracedecay-tool-catalog/
├── Cargo.toml
├── src/
│   ├── lib.rs                         # curated definition/snapshot/resolution API
│   ├── error.rs                       # validation/compiler/resolution errors
│   ├── id.rs                          # stable Capability/UseCase/Intent/Binding IDs
│   ├── definition.rs                  # CapabilityDefinition and UseCaseDefinition
│   ├── schema.rs                      # domain SchemaRef and compatibility schemas
│   ├── effect.rs                      # read/mutate/idempotency/preview/rollback
│   ├── availability.rs                # prerequisites/capability gaps
│   ├── freshness.rs                   # local/live/joined truth requirements
│   ├── privacy.rs                     # sensitivity/access/audit declarations
│   ├── lifecycle.rs                   # active/deprecated/replaced/legacy/pending
│   ├── registry.rs                    # validated immutable built-in registry
│   ├── snapshot.rs                    # canonical encoding, digest, compact facts
│   ├── resolve.rs                     # intent/capability/binding lookup only
│   ├── definitions/
│   │   ├── mod.rs
│   │   ├── project.rs
│   │   ├── code.rs
│   │   ├── graph.rs
│   │   ├── git.rs
│   │   ├── sessions.rs
│   │   ├── lcm.rs
│   │   ├── memory.rs
│   │   ├── policy.rs
│   │   ├── automation.rs
│   │   ├── coordination.rs
│   │   ├── observability.rs
│   │   ├── operations.rs
│   │   └── labs.rs
│   ├── bindings/
│   │   ├── mod.rs
│   │   ├── mcp.rs
│   │   ├── cli.rs
│   │   ├── http.rs
│   │   ├── dashboard.rs
│   │   ├── skill.rs
│   │   └── hook.rs
│   ├── git/
│   │   ├── mod.rs
│   │   ├── intent.rs
│   │   ├── truth.rs
│   │   └── output_semantics.rs
│   ├── generate/
│   │   ├── mod.rs
│   │   ├── canonical_json.rs
│   │   ├── mcp.rs
│   │   ├── cli.rs
│   │   ├── openapi.rs
│   │   ├── typescript.rs
│   │   ├── dashboard.rs
│   │   ├── policy_facts.rs
│   │   └── docs.rs
│   └── audit/
│       ├── mod.rs
│       ├── legacy_manifest.rs
│       ├── diff.rs
│       └── parity.rs
├── src/bin/
│   └── catalog-gen.rs                 # build/CI generator; filesystem allowed here
├── inventory/
│   ├── v1-mcp.json
│   ├── v1-cli.json
│   ├── v1-http.json
│   ├── v1-dashboard.json
│   ├── v1-skills.json
│   ├── v1-hooks.json
│   └── incoming-master.json
├── generated/
│   ├── catalog.json
│   ├── catalog.digest
│   ├── mcp-tools.json
│   ├── cli-bindings.json
│   ├── cli-command-tree.json
│   ├── openapi-operations.json
│   ├── dashboard-commands.json
│   ├── hook-bindings.json
│   ├── policy-routing-facts.json
│   ├── presentations.json
│   ├── output-formats.json
│   ├── errors-and-exit-codes.json
│   ├── aliases-and-cutoffs.json
│   ├── scope-bindings.json
│   ├── effect-bindings.json
│   ├── parity-matrix.json
│   └── capability-reference.md
├── tests/
│   ├── support/mod.rs
│   ├── identity_version.rs
│   ├── definition_validation.rs
│   ├── generation_determinism.rs
│   ├── complete_inventory.rs
│   ├── transport_parity.rs
│   ├── git_routing.rs
│   ├── git_truth_reconciliation.rs
│   ├── output_semantics.rs
│   ├── hint_discovery.rs
│   ├── privacy_security.rs
│   └── compatibility_migration.rs
└── benches/
    ├── snapshot.rs
    └── resolve.rs
~~~

This `generated/` filename set is the canonical artifact home: plan 21 §5.2 consumes exactly these files, and any variant name it lists is the same artifact under this name, not a second output.

Companion generated consumers:

~~~text
crates/tracedecay-policy/src/evaluators/routing.rs
crates/tracedecay-hooks/src/conformance/manifest.rs
crates/tracedecay-application/src/registry.rs
crates/tracedecay-api/src/openapi/generated.json
dashboard/app/src/generated/{catalog.ts,commands.ts}
src/mcp/tools/generated_v2.rs
src/cli/generated_v2.rs
docs/reference/generated-capabilities.md
~~~

Generated files carry a source digest header and are never hand-edited. The public OpenAPI, JSON Schema, and SDK trees are produced through plan 17's contract-IR pipeline (plan 17 §5.1): this catalog contributes `openapi-operations.json` operation metadata to the IR, and plan 17 owns generation of `crates/tracedecay-api/src/openapi/generated.json` and the client packages.

## 7. Dependency Direction and Forbidden Imports

~~~text
tracedecay-domain
        ↑
tracedecay-tool-catalog
        ├──→ tracedecay-policy
        ├──→ tracedecay-application ──→ CLI/MCP/API/dashboard adapters
        └──→ tracedecay-hooks
~~~

The catalog imports only domain/schema/value libraries. The catalog-gen binary may consume serialized legacy inventories and generation libraries; it does not import production servers or execute commands.

Forbidden in production library:

rusqlite, libsql, sqlx, axum, clap Parser/Command execution, rmcp server, reqwest, octocrab, git2, std::fs, std::process, tokio runtime, dashboard packages, root McpServer, and application use-case implementations.

CI verifies no catalog -> application/policy/hooks/store/query/projectors/root edge. Policy/application depend on the catalog, never the reverse.

### Consumes and produces

| Boundary | Consumes | Produces |
|---|---|---|
| `tracedecay-domain` | Schema refs, IDs, scope/sensitivity/evidence/watermark/query/command value contracts | No domain writes or duplicate semantic types |
| Checked definition source | Static capability/use-case/intent/binding definitions and compatibility dispositions | Validated immutable `ToolCatalogSnapshot` and compact route facts |
| Build/audit inventory | Serialized MCP/CLI/HTTP/dashboard/skill/hook/config/incoming-master inventories with commit/version/watermark/digest, plus plan 20's `config-registry-v1.json` descriptor manifest and its `ConfigRegistryDigest` | Drift/parity reports; no runtime filesystem or surface introspection |
| Generators | Validated snapshot plus domain/application schema refs | Deterministic MCP, CLI, OpenAPI, TypeScript, dashboard, hook, policy-fact, and docs artifacts |
| Policy/application/hooks/adapters | No executable callback into consumers | Pinned catalog snapshots, lookup/resolution results, generated binding metadata |

The catalog produces metadata and generated contracts only. It never produces query results, policy decisions, hook replies, application effects, Git/live refreshes, storage rows, or UI render state.

## 8. Stable IDs, Definition, and Binding Contracts

ID grammar:

- CapabilityId: capability.<domain>.<noun>; broad owned capability, rarely changes.
- UseCaseId: usecase.<domain>.<verb-noun>; one semantic request/result/effect contract.
- IntentId: intent.<domain>.<task>; user-task classifier target.
- BindingId: binding.<surface>.<stable-name>; one exposed surface.
- PresentationId: presentation.<domain>.<view>; one reviewed human presentation spec (plan 21 §7).
- Versions are separate SemVer fields. IDs never embed v1/v2 or transport names except BindingId.

This crate's `id.rs` owns all five ID kinds; plan 21 consumes `PresentationId` without minting a parallel grammar.

Examples:

- capability.git.branch-intelligence;
- usecase.git.list-branches;
- intent.git.branch-inventory;
- binding.mcp.branch_list;
- binding.cli.branch.list;
- presentation.git.branch-inventory.

Coordination IDs are current V2 definitions, not compatibility aliases:

- `capability.agent.coordination`;
- `usecase.agent.publish-presence`;
- `usecase.agent.claim-work`;
- `usecase.agent.heartbeat-presence`;
- `usecase.agent.find-nearby-work`;
- `usecase.agent.acknowledge-overlap`;
- `usecase.labs.replay-coordination`.

They declare profile/activity ownership, <=160-character safe-summary schema, retrieval-anchor privacy, heartbeat/TTL/status, repository/worktree/ref/PR/file/symbol/query scopes, read/write intent, redundancy modes, cursor/cap semantics, and effect owner. `find-nearby-work` is bounded to 100 and read-only. Claim/ack mutations are idempotent/audited. The catalog never grants cancellation/reassignment/lock/message authority.

~~~rust
pub struct CapabilityDefinition {
    pub id: CapabilityId,
    pub version: Version,
    pub owner: BoundedContext,
    pub title: CatalogText,
    pub summary: CatalogText,
    pub intents: BTreeSet<IntentId>,
    pub aliases: BTreeSet<CatalogAlias>,
    pub use_cases: BTreeSet<UseCaseId>,
    pub lifecycle: CapabilityLifecycle,
    pub availability: AvailabilitySpec,
    pub privacy: PrivacySpec,
    pub audit: AuditSpec,
}

pub struct UseCaseDefinition {
    pub id: UseCaseId,
    pub version: Version,
    pub capability: CapabilityId,
    pub request_schema: SchemaRef,
    pub response_schema: SchemaRef,
    pub error_schema: SchemaRef,
    pub scopes: BTreeSet<ScopeKind>,
    pub scope_selector_schema: Option<SchemaRef>,
    pub effects: EffectSpec,
    pub idempotency: IdempotencySpec,
    pub pagination: PaginationSpec,
    pub streaming: StreamingSpec,
    pub cost: CostClass,
    pub latency: LatencyClass,
    pub freshness: FreshnessRequirement,
    pub evidence: EvidenceOutputSpec,
    pub required_input_trust: InputTrustSpec,
    pub limits: LimitSpec,
    pub bindings: BTreeSet<BindingId>,
}

pub struct SurfaceBinding {
    pub id: BindingId,
    pub surface: SurfaceKind,
    pub use_case: UseCaseId,
    pub name_or_route: SurfaceInvocationCode,
    pub request_mapping: MappingRef,
    pub presentation: PresentationId,
    pub availability_override: Option<AvailabilitySpec>,
    pub compatibility: CompatibilityDisposition,
}

pub enum SurfaceKind {
    Cli,
    Mcp,
    Http,
    Sdk,
    Dashboard,
    Hook,
    Skill,
    Automation,
    Executor,
    ContextScout,
    InternalHost,
}

pub enum ExecutionModeV2 {
    ReadOnly,
    DirectCommit,
    ConfirmedDestructive,
    AutonomousPolicyEffect,
    ResumableWorkflow,
    InternalHostLifecycle,
}

pub struct EffectSpec {
    pub execution_mode: ExecutionModeV2,
    pub effect_owner: BoundedContext,
    pub side_effects: BTreeSet<EffectKind>,
    pub preview: PreviewSupport,
    pub confirmation: ConfirmationRequirement,
    pub recovery: RecoveryDisposition,
}

pub struct IdempotencySpec {
    pub idempotent: bool,
    pub key: IdempotencyKeyRequirement,
    pub expected_version: ExpectedVersionPolicy,
    pub retry_receipt: RetryReceiptPolicy,
}
~~~

`SurfaceKind` is the one closed, generated surface vocabulary for binding identity and usage accounting. Stable integer codes and `snake_case` wire names are emitted with the catalog snapshot; plans 21 and 26 consume those generated values and may not maintain SQL-, renderer-, or telemetry-local surface lists. A genuinely new surface requires a catalog-schema version, compatibility disposition, accounting classification, and conformance fixtures before any binding can use it.

`ExecutionModeV2` lives in this crate's `effect.rs` and is the only closed effect-mode enum; plan 21 §11.1 consumes it for surface annotations and defines no surface-local variant. `SurfaceInvocationCode` carries the current canonical surface name or route only; V1 names live solely inside `CompatibilityDisposition` (field contract defined in plan 21 §17.1) and `CatalogAlias` provenance rows. `PresentationId` replaces any binding-local view reference; presentation descriptors themselves are plan 21's.

`CatalogAlias` is an intent/search/provenance label inside a snapshot, not a callable MCP/CLI/HTTP/hook binding name. Only `SurfaceBinding` can be invoked, and generation includes only bindings active in the current protocol epoch — the `(schema_version, catalog_generation)` pair pinned in `ToolCatalogSnapshot` (Section 9).

Validation fails on:

- duplicate ID/binding/name/method+route;
- unknown schema/intent/capability/use-case;
- binding request/response fields not losslessly mapped;
- mutation without execution-mode/effect-owner/idempotency/audit/confirmation/recovery disposition;
- destructive effect presented as read or implicit dry-run;
- query/list without bounded pagination/cap;
- live/semantic/joined Git output without freshness/evidence;
- diagnostic/hint route whose required typed input trust can be satisfied by arbitrary prompt/log text;
- sensitive output without access/redaction rules;
- deprecated item without replacement/end window;
- skill/hook route to unavailable or incompatible host capability;
- transport-only semantics not represented in the use-case definition;
- scoped binding without `ScopeSelectorV2`, or any current-project/CWD/first-match/base-checkout/current-graph fallback;
- current inventory item with no owner/parity disposition.

## 9. Immutable Catalog Snapshot and Runtime Resolution

~~~rust
pub struct ToolCatalogSnapshot {
    pub schema_version: CatalogSchemaVersion,
    pub catalog_version: Version,
    pub catalog_generation: CatalogGeneration,
    pub built_from_commit: CommitDigest,
    pub definitions: BTreeMap<CapabilityId, CapabilityDefinition>,
    pub use_cases: BTreeMap<UseCaseId, UseCaseDefinition>,
    pub bindings: BTreeMap<BindingId, SurfaceBinding>,
    pub intent_routes: BTreeMap<IntentId, Vec<RouteCandidate>>,
    pub source_manifests: Vec<InventoryManifestRef>,
    pub config_registry_digest: ConfigRegistryDigest,
    pub digest: ContentDigest,
}

pub struct AvailabilityContext {
    pub host: Option<HostKind>,
    pub profile: ProfileId,
    pub scope: ScopeSelectorV2,
    pub scope_resolution: ScopeResolutionV2,
    pub indexed_refs: BTreeSet<RefId>,
    pub installed_bindings: BTreeSet<BindingId>,
    pub credentials: BTreeSet<CredentialCapability>,
    pub privacy_access: AccessDigest,
    pub local_watermark: VectorWatermark,
    pub live_delivery: Option<LiveDeliveryWatermark>,
}

pub fn resolve_intent(
    snapshot: &ToolCatalogSnapshot,
    intent: IntentId,
    context: &AvailabilityContext,
) -> RouteResolution;

pub struct RouteCandidate {
    pub use_case: UseCaseId,
    pub binding: BindingId,
    pub availability: AvailabilityDecision,
    pub evidence_source: FreshnessRequirement,
    pub fallback_rank: u16,
    pub rationale: Vec<RouteReason>,
}

pub struct RouteResolution {
    pub intent: IntentId,
    pub selected: Option<RouteCandidate>,
    pub alternatives: Vec<RouteCandidate>,
    pub unavailable: Vec<RouteCandidate>,
    pub catalog_digest: ContentDigest,
}
~~~

`catalog_generation` is the monotonic per-daemon-generation counter negotiated in the MCP handshake exactly as master plan §2.6 (merged #422) and plans 12/21 describe: a daemon increments it whenever it activates a different snapshot digest, clients pin the `(digest, catalog_generation)` pair, and the daemon emits at most one bounded `tools/list_changed` refresh per client per daemon generation. Generated MCP server metadata must declare the `tools.listChanged` capability. A client holding a stale generation fails closed with plan 17's typed `client_update_required`/`daemon_restart_required`/`capability_replaced` codes naming the current binding; it never receives a silently different tool set. `config_registry_digest` pins plan 20's registry manifest that this snapshot was built from.

RouteResolution returns ranked available candidates, unavailable candidates with exact gaps, required freshness/evidence source, safe fallbacks, expected cost/latency, and catalog digest. It does not classify natural language or invoke tools.

`AvailabilityContext.scope` is the exact shared selector and `scope_resolution` is the matching catalog/store snapshot pinned at `local_watermark`. Route resolution preserves every selected repo/project/checkout/worktree/ref/snapshot/generation tuple, returns ambiguity/stale/quarantine coverage, and never narrows to `project_key`, first CWD, active base checkout, current branch graph, or registry first match. A route that cannot honor the selector is unavailable, not a candidate with guessed scope.

Policy receives compact facts selected by IntentId/category:

- stable IDs/names/aliases and one-sentence task fit;
- required scope/host/index/live refresh/credentials;
- read/mutate/manual-autonomous/confirmation/dry-run;
- local semantic/live delivery/joined truth;
- fallback and overlap priority;
- compact parameter requirements;
- catalog/version/digest.

Compact facts have a token budget and digest. Full descriptions/examples remain discoverable by explicit catalog query.

## 10. Generated Outputs and One-Source Parity

Generation pipeline:

1. Validate canonical domain schema registry and typed definitions.
2. Load frozen legacy inventory manifests for MCP/CLI/HTTP/dashboard/skills/hooks/config, plus plan 20's generated `config-registry-v1.json` descriptor manifest; pin its `ConfigRegistryDigest` in the snapshot.
3. Require owner/use-case/binding/lifecycle mapping for every inventory row.
4. Canonically sort and encode catalog JSON; compute digest.
5. Generate MCP definitions, CLI binding metadata/help links, OpenAPI operation metadata, TypeScript types, dashboard commands, hook bindings, compact policy facts, and docs.
6. Reparse every artifact and compare semantic schemas/mappings back to the source definitions.
7. Fail if generated worktree differs in CI.

The generator never manufactures business validators. Request schemas reference domain/application contract schemas. V1 adapter mappings may use frozen compatibility schemas until their use case moves to V2.

Semantic parity checks:

- required/optional/default/enum/range match;
- scope/profile/project/ref semantics match;
- ordering/cursor/cap/truncation/coverage match;
- evidence/confidence/freshness/watermark match;
- errors/status/restartability match;
- read/mutate/execution-mode/effect/idempotency/confirmation/dry-run/recovery match;
- secret/redaction/export behavior match;
- direct_user/subagent/tool_result/#410 representative filters match;
- JSON typed result matches before Markdown/CLI/UI rendering.

## 11. Git Intent, Tool Routing, and Truth Reconciliation

Required Git routes:

| Intent | Primary binding/use case | Required truth | Overlap rule |
|---|---|---|---|
| Branch inventory | branch_list / usecase.git.list-branches | Local indexed generations, tracking/fallback, ref/index watermark | Not live remote branch truth; show refresh/fallback state. |
| Search another branch | branch_search / usecase.code.search-branch-symbols | Local named immutable graph generation | Exact branch generation required; no current-branch fallback without label. |
| Compare branch/code impact | branch_diff / usecase.git.compare-semantic-branches | Local base/head/merge base plus graph generations | Prefer over raw text diff for semantic impact; reconcile changed-file universe. |
| Review pull request | pr_context / usecase.delivery.review-pr-context | Joined local semantic and separately fetched live PR/check/review | Prefer over branch_diff when PR intent includes live state; preserve both watermarks. |
| Draft changelog/release notes | changelog / usecase.delivery.draft-changelog | Local commit/PR evidence plus declared live inputs | Output is proposal; exact ref range required. |
| Investigate commit | commit_context / usecase.git.inspect-commit | Local commit/tree/symbol/session evidence | Live remote presence/check state is separate. |
| Attribute sessions | sessions_for / usecase.git.find-correlated-sessions | Local correlation projection/evidence/confidence/health | Absence is coverage, not proof no session. |
| Attribute workflow/agents | workflows / usecase.agent.find-correlated-workflows | Local captured workflow/session projection | Prefer over sessions_for when parent/agent workflow intent is explicit. |

Live/local reconciliation contract:

~~~rust
pub struct GitTruthDescriptor {
    pub source: GitTruthSource,
    pub repository: RepositoryId,
    pub base: Option<CommitId>,
    pub head: Option<CommitId>,
    pub merge_base: Option<CommitId>,
    pub normalized_changed_files_digest: Option<ContentDigest>,
    pub changed_files_count: Option<u64>,
    pub changed_files_complete: bool,
    pub fetched_or_indexed_at: UtcMicros,
    pub watermark: TruthWatermark,
    pub fallback: Option<FallbackState>,
}

pub enum ChangeMembership {
    DirectlyChanged { file_hunk_or_symbol_evidence: Vec<EvidenceRef> },
    StructurallyImpacted { graph_path: Vec<EntityRef>, confidence: Confidence },
    CandidateTest { attribution: EvidenceClass, reason: TestSelectionReason },
    ContextOnly { reason: ContextReason },
}
~~~

The PR #410 planning audit is a required regression fixture: pr_context agreed with live state on 16 changed files and merge base, yet expanded to roughly 2,866 modified symbols and a huge test universe. V2 must:

- never report a symbol as DirectlyChanged without changed file/hunk/symbol evidence;
- put signature/body/occurrence changes supported by diff mapping in DirectlyChanged;
- put caller/dependent/neighbor/transitive fanout in StructurallyImpacted with graph path/depth/algorithm/version/confidence;
- put static/dynamic/heuristic test attribution in CandidateTest with evidence and caps;
- put orientation/support rows in ContextOnly;
- report per-class counts, cap/truncation, universe, exclusions, and watermarks;
- cap breadth/depth and allow the caller to request another bounded expansion;
- keep direct changed-file truth separate from graph-derived impact even when rendered together.

RevisionReconciliation is Aligned only when repository/base/head/merge-base and complete normalized changed-file digest agree. LocalOnly, LiveOnly, Drifted, Capped, Stale, and Incompatible return named actions RefreshLive, ReindexLocal, RecomputeBoth, or NarrowScope. Drifted inputs cannot support joined conclusions.

## 12. Discovery, Hints, and Missed Capability Feedback

The planning-session correction becomes a checked fixture:

- prompt mentions create/update worktree from master, open PRs, branches, prior implementation intent;
- expected high-confidence routes include branch_list/pr_context/changelog/sessions_for/workflows plus live GitHub refresh where current PR/check state is requested;
- generic shell/GitHub-only exploration without catalog consideration yields MissedCapability candidate;
- the user's correction records HumanCorrection with corrected intent/route and supporting event;
- correction does not automatically mean an emitted hint was bad; policy evaluates prior route/silence/evidence.

For every eligible prompt policy records:

- catalog snapshot/digest and host availability;
- intents and capability candidates considered;
- selected/suppressed/unavailable/fallback routes and reasons;
- whether a hint was delivered;
- observed invocation/result and evidence class;
- missed high-value capability;
- human correction;
- terminal horizon/coverage.

Useful silence remains valid when confidence/value is below threshold, the tool is unavailable, the user already selected it, or repetition/token/privacy cost dominates. Discovery metrics use separate denominators for eligible opportunities, hints emitted, tools invoked, missed capability, correction, unavailable, and unresolved.

No hook injects the full 104-tool catalog. It injects compact category/intent facts or a discovery command when needed.

Agent-coordination route facts are even narrower: eligible only at session start, subagent start, pre-edit, catalog-declared expensive research, or material scope change. The route requires current presence/claim capability, a nearby-agent query, typed anchors plus any available safe summary, and policy evaluation; it emits at most one compact advisory hint. Planned ensemble/diverse-review/shared/sequential redundancy, acknowledgement, cooldown, partial coverage, or unchanged scope are explicit suppression facts. Catalog analytics keep separate eligible/emitted/suppressed/acted/handoff/duplicate-avoided/false-positive/unresolved denominators.

## 13. Inventory Extractors and Drift Gates

V1 extractor inputs:

- MCP: registered definition/schema/handler/renderer set, not source regex alone;
- CLI: recursive clap::CommandFactory tree including aliases/hidden/deprecated/options/env/defaults;
- HTTP: a typed legacy route registry wrapping every Axum method/route/gateway target; raw Router is compared during migration;
- dashboard: generated command/action manifest plus plugin registry and API calls; audit data-action/test IDs and handler bindings;
- skills: profile/bundled/installed manifests with checksums/lifecycle/targets/references, not instruction contents;
- hooks: provider descriptor registry and installer manifests/event matchers/effects;
- config/operations: every mutation handler/command and its execution-mode/preview-or-autonomy/confirmation/audit/recovery behavior;
- incoming PRs: refreshed semantic and live changed-file inventories with merge-base/head.

CI fails when:

- a current inventory row has no binding/disposition;
- a catalog binding has no surface or is mapped twice incompatibly;
- request/result schemas drift;
- a route/command/tool is renamed without alias/deprecation;
- a mutation lacks effect metadata;
- generated files/docs differ;
- policy/hook/client embeds an unknown catalog digest;
- Git result omits membership/evidence/freshness/caps;
- #410 filters differ between surfaces;
- #405/#407 migration aliases appear as separate active capabilities/profiles;
- #409 appears as required future-master behavior.

Every accepted drift updates the inventory manifest, definition version, generated artifacts, migration/parity fixture, and changelog in the same commit.

## 14. Compatibility, Privacy, and Security

- Catalog text is safe static metadata. User prompts, queries, paths, repository names, fact/skill content, tool arguments/results, credentials, and payloads never enter definitions/manifests/metrics.
- Availability reports credentials by capability/presence only, never value.
- Generated HTTP/MCP/CLI/dashboard descriptions are escaped for their target; fuzz markup/control characters/JSON schema/reference cycles.
- Verify artifact digest and source manifests before policy/hook use. Unknown/incompatible major version fails closed with a named capability gap.
- Catalog publication is stage -> validate -> hash -> immutable store -> CAS active pointer. Readers pin one full snapshot.
- Preserve old snapshots while referenced by policy evaluations, hint deliveries, replay fixtures, exports, skills, migration receipts, or the data rollback window; snapshot retention never activates their bindings.
- At cutover, only current bindings are generated or discoverable. V1 bindings remain historical inventory/replay evidence, not active aliases. Stale clients fail exact protocol/catalog-generation checks with plan 17's typed `client_update_required`/`daemon_restart_required`/`capability_replaced` codes naming the current capability ID/name.
- Destructive bindings never become available through a read-only host/skill merely because names match.
- Managed skill references are validated against catalog IDs/versions/host targets at candidate creation, autonomy decision, materialization, use, recovery, and replay; no per-item approval/install binding is emitted.

## 15. Performance and Quality Gates

- Build/validate/generate the full current catalog in <=2 s and <=256 MiB on the reference machine.
- Load/canonical-verify snapshot in <=25 ms p95; exact ID lookup <=50 microseconds p95; route resolution over one intent <=250 microseconds p95.
- Compact hint routing facts for one intent/category <=1 KiB by default and <=4 KiB hard cap.
- Coordination route facts include no summaries/agent IDs, fit <=512 bytes, expose only five allowed trigger classes, and cannot resolve to a cancellation/reassignment/message effect.
- 10,000 concurrent readers during 100 snapshot publications see one complete digest each; no mix.
- Generation is byte-identical across clean runs/platform path differences/time zones/map insertion orders.
- 100% of live inventory rows have owner/use-case/binding/lifecycle; zero unexplained drift.
- 100% of mutations have effect/idempotency/audit/execution-mode/confirmation-or-autonomy/recovery disposition; 0 curation candidates have per-item preview/approve/apply/reject/rollback bindings.
- 100% of Git rows have truth source/freshness/watermark/membership/evidence/cap; zero transitive row labeled direct.
- Secret corpus produces zero secret-bearing catalog/generated/docs/metric output.
- New production files <=800 lines; definitions are split by bounded context.

## 16. PR 22A TDD and Commit Sequence

Commands run from repository root with checkout-local target directories.

### Commit 1: Pure IDs, definitions, validation, and immutable snapshots

**Files:** workspace/Cargo.toml; crate Cargo.toml; src/{lib,error,id,definition,schema,effect,availability,freshness,privacy,lifecycle,registry,snapshot,resolve}.rs; tests/{identity_version,definition_validation,generation_determinism,privacy_security}.rs.

- [ ] Write failing tests for stable IDs, canonical digest, unknown references, duplicate bindings, unbounded list, mutation metadata, secret text, deprecation/replacement, incompatible major, and concurrent pinned snapshots.
- [ ] Run cargo test -p tracedecay-tool-catalog --test identity_version --test definition_validation --test generation_determinism --test privacy_security. Expected: fail because crate/types do not exist.
- [ ] Implement Sections 7–9 with canonical sorted encoding and pure resolution.
- [ ] Re-run. Expected: all tests pass; insertion order/time zone/path syntax do not change digest.
- [ ] Commit: feat(catalog): define versioned capability contracts.

### Commit 2: Freeze complete V1 inventories

**Files:** inventory/*.json; src/audit/{mod,legacy_manifest,diff,parity}.rs; src/bin/catalog-gen.rs; tests/complete_inventory.rs.

- [ ] Build typed MCP, recursive CLI, HTTP, dashboard, skill, hook, and mutation extractors; capture every Section 5 row with binary/commit/time/watermark/digest.
- [ ] Add failing test every_legacy_surface_has_one_disposition and exact current count/name anchors, while allowing an explicit refreshed-manifest review when master changed.
- [ ] Run cargo test -p tracedecay-tool-catalog --test complete_inventory. Expected: fail with the complete unmapped row list.
- [ ] Add owner/use-case/binding/lifecycle dispositions, including removed Hermes aliases and closed #409 history.
- [ ] Re-run. Expected: no unmapped or duplicate row.
- [ ] Commit: test(catalog): freeze complete TraceDecay surface inventory.

### Commit 3: Define every capability and #410 filter parity

**Files:** src/definitions/*.rs; src/bindings/*.rs; tests/{complete_inventory,transport_parity,compatibility_migration}.rs.

- [ ] Add definitions for all project/code/graph/Git/session/LCM/memory/policy/automation/observability/operation/lab surfaces and all 104 source MCP definitions with dispositions, including `ast_grep_search` and `move_symbol`; 103 are installed at 0.0.47.
- [ ] Add current V2 coordination definitions/bindings for presence, claim, heartbeat, nearby work, overlap acknowledgement/handoff, analytics, and Coordination Lab. Fixture-lock parent prefix `019f4906`, four PR #359 child agents, and Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d`; these are evidence anchors, never catalog text.
- [ ] Add direct_user/subagent/tool_result/parent-representative schema fixtures for message search, LCM, CLI, MCP, future HTTP/dashboard/export/saved view.
- [ ] Run tests. Expected: fail until every legacy field/effect/error is mapped.
- [ ] Complete definitions/mappings and explicit missing-surface dispositions.
- [ ] Re-run. Expected: semantic parity passes; every sanitized native row remains available.
- [ ] Commit: feat(catalog): catalog every current TraceDecay use case.

### Commit 4: Git routing, reconciliation, and output semantics

**Files:** src/git/*.rs; src/definitions/git.rs; tests/{git_routing,git_truth_reconciliation,output_semantics,hint_discovery}.rs.

- [ ] Add one route fixture per eight Git tools plus multi-repo/worktree selector preservation, `sessions.project_key` conflict, Claude first-CWD ambiguity, active-base-versus-PR-worktree graph mismatch, ignored dependency hint retaining scope, stale registry/store pollution, unavailable/fallback, local/live/joined, force-push/drift/cap/stale cases.
- [ ] Add the planning correction and #410 16-file/2,866-symbol/test-fanout regression fixtures.
- [ ] Run focused tests. Expected: fail while outputs conflate changed/impacted/tests/context or omit truth metadata.
- [ ] Implement Sections 11–12.
- [ ] Re-run. Expected: every row classified/evidenced/capped; drift blocks joined conclusion; routing selects semantic Git tools before generic fallbacks when appropriate.
- [ ] Commit: feat(catalog): route and reconcile Git intelligence.

### Commit 5: Generate transport, policy, dashboard, and docs artifacts

**Files:** src/generate/*.rs; generated/*; dashboard/app/src/generated/*; docs/reference/generated-capabilities.md; tests/{generation_determinism,transport_parity}.rs.

- [ ] Add golden tests for MCP/CLI/OpenAPI/TypeScript/dashboard/hook/policy/docs outputs and reparse parity.
- [ ] Run tests. Expected: fail before generators exist.
- [ ] Implement deterministic generation and source-digest headers.
- [ ] Run generator twice from clean output and compare hashes. Expected: byte-identical.
- [ ] Re-run tests. Expected: all generated requests/results/effects/errors map losslessly.
- [ ] Commit: feat(catalog): generate capability surfaces from one source.

### Commit 6: Wire current adapters, internal V1 differential harness, and drift enforcement

**Files:** src/mcp/tools/generated_v2.rs; src/cli/generated_v2.rs; typed legacy route/action/hook registries; CI scripts/workflows; tests/complete_inventory.rs.

- [ ] Add CI tests that deliberately register one uncataloged tool/command/route/action/hook and assert a named failure.
- [ ] Make current surfaces consume generated descriptions/schema/metadata; keep V1 handlers reachable only from the internal differential/shadow harness and never from live dispatch after cutover.
- [ ] Run existing MCP/CLI/dashboard/hook/skill/config suites plus catalog drift tests. Expected: all pass.
- [ ] Regenerate from refreshed master and require clean git diff.
- [ ] Commit: refactor(catalog): enforce generated capability parity.

### Commit 7: Shadow policy/hook adoption and migration receipt

**Files:** policy routing fixtures/bundle manifests; hook conformance manifests; migration receipts/tests.

- [ ] Run old classifier/routing and new catalog-backed policy in shadow on the versioned prompt corpus.
- [ ] Compare candidates/routes/unavailable/fallback/silence/missed/correction, latency/token cost, and Git truth requirements.
- [ ] Block cutover on an unexplained capability omission, noisy regression, stale/local-live conflation, or output-membership error.
- [ ] Publish catalog digest, V1 inventory digests, accepted differences, feature flags, rollback, and retained snapshot list.
- [ ] Commit: refactor(catalog): make generated catalog authoritative.

## 17. Cutover, Rollback, and Deletion Criteria

Cut over catalog consumers independently:

1. docs/reference and explicit catalog query;
2. MCP/CLI descriptions and schema metadata;
3. dashboard command palette/action manifests;
4. skills validation/references;
5. hook descriptors and availability;
6. policy routing/hints;
7. generated adapter registration.

At each step, a feature flag selects the old registry/router or pinned new snapshot. Rollback restores the prior catalog digest and old metadata owner; use-case implementation/data remains unchanged. Recorded evaluations keep their original digest.

Delete a hand-maintained definition/routing list only when:

- its complete old inventory is fixture-locked;
- generated current output has passed the bounded shadow/cutover/rollback window;
- no host/plugin/skill references the old name without an alias;
- schema/effect/error parity and rollback are proven;
- archived replay can load the old catalog snapshot;
- drift CI proves new entries cannot bypass the catalog;
- closed #409 and removed Hermes paths remain historical aliases only.

Never delete raw #410 prompt rows or collapse evidence in the catalog. Retire only duplicate surface-specific filter logic after shared semantic parity.

## 18. Final Verification

- [ ] cargo fmt --check. Expected: exit 0.
- [ ] cargo clippy -p tracedecay-domain -p tracedecay-tool-catalog --all-targets -- -D warnings. Expected: exit 0.
- [ ] cargo test -p tracedecay-tool-catalog --all-features. Expected: all tests pass, none ignored.
- [ ] Run current MCP, CLI parse/help, dashboard route/action, skill lifecycle, hook/installer, policy routing, project/profile migration, session/LCM search, Git context, renderer, and config mutation suites. Expected: compatibility passes.
- [ ] Run catalog-gen twice, validate all schemas/artifacts, and git diff --exit-code generated docs/reference. Expected: deterministic clean output.
- [ ] Compare live inventory to generated catalog. Expected: 100% mapped, zero duplicate/unowned/incompatible row.
- [ ] Run Git routing/truth/output regression corpus including #410. Expected: correct tool, separated truth, direct/impact/test/context membership, evidence/caps.
- [ ] Run #410 filter parity across CLI/MCP/generated HTTP/dashboard/export schemas. Expected: identical semantics and raw-row coverage.
- [ ] Run benchmark/concurrent-publication/privacy/fuzz gates from Sections 14–15. Expected: all pass.
- [ ] cargo tree -p tracedecay-tool-catalog --edges normal and forbidden-import scan. Expected: no application/store/query/policy/hook/server/UI execution dependency.
- [ ] Run the placeholder scan using split regex atoms: rg -n 'TB[D]|TO[D]O|\bimplement lat[e]r\b|\bfill i[n]\b|\bappropriate erro[r]\b|\bsimilar to Tas[k]\b' docs/superpowers/plans/tracedecay-v2/08-tool-catalog-crate.md. Expected: no matches.

## 19. Definition of Done

- Every current and incoming-master capability has one stable owner/use case/version and explicit surface/lifecycle mapping; all 104 source MCP definitions carry dispositions (103 installed at 0.0.47; 102 at the older frozen inventory).
- MCP, CLI, HTTP, dashboard, skills, hooks, policy hints, generated docs, and clients share semantic schemas/effects/errors without copy drift.
- The right TraceDecay Git capability is discoverable at the right intent, with live/local truth and output membership impossible to confuse.
- #405/#407 ownership, #410 filtering/dedupe, #411 remediation ownership, and #412 lifecycle prerequisites are cataloged; #413 contributes actual release/protocol version; #409 remains historical only.
- Missed capability and human correction are replayable evidence, while useful silence remains measurable.
- Presence/claim/nearby/ack/handoff/Coordination-Lab capabilities are current, bounded, privacy-safe, trigger-constrained, advisory, planned-redundancy-aware, and impossible to confuse with agent-control authority.
- Every scoped binding consumes the same `ScopeSelectorV2` plus pinned `ScopeResolutionV2`; multi-repo/project/checkout/worktree/ref/snapshot/generation selections and ambiguity/staleness remain visible and no surface invents a current-project/base-checkout/current-graph fallback.
- Catalog generation is deterministic, compact, privacy-safe, versioned, replayable, and enforced by CI.
- The catalog contains no business execution, storage, query, network, Git, host, or UI implementation.
