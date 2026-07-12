# TraceDecay V2 Tool Catalog Crate Implementation Plan

**Goal:** Create one versioned, generated capability catalog that makes every TraceDecay use case discoverable and semantically consistent across MCP, CLI, HTTP, dashboard, skills, hooks, policy routing, documentation, and compatibility migration.

**Architecture:** tracedecay-tool-catalog is a pure metadata/compiler crate. Checked-in typed use-case definitions reference domain schemas and declare ownership, effects, scope, freshness, privacy, cost, evidence, compatibility, and transport bindings; generators emit immutable manifests and adapter metadata, while audit extractors compare every live/legacy surface against the catalog. For MCP, the catalog generates the complete protocol-facing tool/resource/resource-template/prompt/completion manifest plus immutable exposure profiles, capability requirements, JSON Schema 2020-12 input/output schemas, annotations, task support, and list-generation facts; the one root MCP adapter owns protocol lifecycle and invokes application ports. Skills plus the generated CLI are the universal host baseline; MCP is an optional generated projection of the same catalog, never a second implementation or semantic API. The crate never executes a use case, performs discovery I/O at runtime, negotiates a connection, or becomes a second application layer.

**Tech Stack:** Rust 2024; serde/serde_json; schemars/jsonschema; semver; blake3; thiserror; clap Command introspection in a build/audit binary only; OpenAPI schema fragments; TypeScript/JSON generation; insta/proptest; V2 domain contracts.

---

## 1. Contract Lock

This plan owns master-plan PR 22A. It lands before tracedecay-policy PRs 23A–23G so policy bundles can pin a catalog digest, and before hook PR 24F so host descriptors bind to stable capabilities.

One generated `HostIntegrationManifestV1` joins capture, hook, installation, MCP/tool, and executor facets by canonical host/version/identity/capability/event codes. Plans 03, 07, 12, and 24 keep separate implementation traits, but none maintains another host-name, tool-permission, hook-point, install-path, config-format, or conformance registry. The accepted-base source inventory has 15 `AgentIntegration` implementations and nine `install_mcp_server` functions—including exact Cline/Roo duplicates—as migration inputs; PR 22A regenerates and drift-checks those counts rather than freezing them as timeless. Descriptor-driven shared install/update/uninstall/config mutation replaces their mechanical code while true host behavior remains explicit. Installation is one generated component set: optional `CoreSkillsCli` plus zero or more `McpFacade { registration, profile }` companions. Core is the portable default on shell-capable hosts; each facade launches the same thin `tracedecay` integration binary/adapter/catalog and connects to the private `tracedecayd` authority. A headless facade-only set is an explicit deployment choice, not a parallel workflow implementation.

Plan 27 extends this same manifest with `HostBundleProjectionFacetV1` references for portable workflows/skills, explicit recipes, specialist roles, hook intents, package components, host overlays, fallback use cases, and conformance cases. It does not add another semantic manifest. The pure `host_bundles` module compiles unsigned deterministic per-host/package `HostBundlePayloadV1` artifact indexes, rendered trees, source maps, omissions, capability differences, release-scan inputs, and conformance inputs; those outputs reference this manifest/catalog digest and contain no copied workflow/effect/grant/task semantics. PR 36R release orchestration scans/rebuilds/conformance-tests/signs the payload into `HostBundleManifestV1` and publishes it. Runtime probing/configuration/install effects remain structurally absent from this crate and enter only through plan 09's application lifecycle plus root `v2::host_deploy` port implementation.

MCP exposure is deliberately smaller than the complete catalog. Generation produces three logical trust-boundary registrations, all backed by the same thin `tracedecay` integration binary, private `tracedecayd` application port, and `BindingId` rows: `tracedecay-context` admits the read-only `agent-core`, `developer`, and `research` profiles; `tracedecay-work` admits the `task-worker` and `orchestrator` profiles; `tracedecay-operator` admits only explicitly installed `operator` and `admin-lab` profiles. Every profile is a reviewed explicit set of `BindingId` values—never a prefix, category glob, future-family wildcard, or runtime allowlist—and carries effect/grant/host-feature/tool-count/definition-token ceilings. A connection pins one registration/profile/digest. Its visible primitive set is exactly `profile bindings ∩ negotiated host support ∩ authenticated grant ceiling ∩ current authorization`; no broader token, alias, root, plugin, or prompt can widen it.

Profile selection is installation/session state, not per-turn progressive disclosure. MCP `listChanged` reports only an actual catalog, availability, or authorization change inside the pinned profile; it cannot activate another profile or reveal a tool for the current prompt. A host's deferred tool search may lazily materialize definitions already inside that profile, but correctness and discoverability never depend on it. Eager-list hosts must pass the same role corpus and definition-token budgets. Skills, hints, CLI help, and API discovery route by stable intent/use-case IDs and may recommend the exact CLI/API alternative when a binding is intentionally outside the installed MCP profile; the catalog generates no generic hidden `invoke`/god tool.

Catalog infrastructure itself reuses plan 01's registry manifest/canonical-encoding substrate for IDs, versions, owners, schemas, deprecations, cross-references, and digests. This crate owns capability semantics and generators, not a second generic registry engine.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) contributes initiative/plan/task/query/control/lifecycle, executor registration/protocol, scheduler, packet, and task-view capability families. This catalog owns their audience/effect/scope/grant/privacy/egress/idempotency/output metadata and generated bindings; `all/*` and generic tool grants never enable task mutations implicitly. The generated inventory imports every plan-24 operation ID and proves a bijection; it cannot retain only task detail while omitting control, executor, scheduler, graph, or protected-share operations:

```text
initiatives.list|get|graph|create|update|pause|resume|retire
plans.list|get|diff|create_version|activate|decompose
work_items.list|get|query|context|dependencies
work_items.create|update|replace|retire|link|unlink|assign|reassign|assign_set
work_items.pause|resume|cancel|archive|retry
work_items.record_attestation|record_review|record_decision|record_exception
work_items.handoff|reopen|reverse_transition
attempts.list|get|timeline
attempts.heartbeat|progress|complete|block
task_offers.list|get|accept|decline|revoke
context_packets.list|get|accept
task_notifications.list|get|create|update|delete
executors.list|get|match|register|heartbeat|drain|unregister
scheduler.status|explain|pause|resume|run_once
saved_views.list|get|create|update|delete|share.plan|share.start|share.revoke
task_graph.status|doctor|events
task_graph.edit_bundles.export|get|validate|diff|rebase|submit|delete
```

Plan [`28-remote-multi-machine-shared-brain.md`](28-remote-multi-machine-shared-brain.md) contributes topology/node/enrollment/placement/sync/replica/backup/failover/repository-correlation use cases. The catalog marks safe status reads separately from operator effects, generates the plan-21 CLI and plan-10/17 API/SDK bindings, includes compact `brain_status` only in reviewed context profiles, and keeps enrollment/revocation/placement/repair/promotion mutations in explicit operator profiles. No generated schema exposes database paths/URLs, sync chunks, credentials, node keys, or Tailscale-specific semantics.

```text
brain.status.get
brain.topology.get
brain.nodes.list|get
brain.join|leave
brain.nodes.rotate|revoke
brain.placements.list|plan|apply|verify
brain.sync.status|run|pause|resume|repair
brain.replicas.list|seed|verify|retire
brain.backup.status|verify
brain.failover.plan|promote|verify
brain.repositories.candidates|adopt|split
```

The `|` notation in this plan is presentation-only shorthand. Catalog generation expands it before validation into one checked-in definition row per canonical `UseCaseId`—for example `brain.nodes.list` and `brain.nodes.get`, never a runtime compound ID—and each row has exactly one application owner plus explicit CLI, HTTP, Rust/TypeScript/Python SDK, dashboard, and audience-filtered MCP binding or a reviewed unavailability disposition. The expanded family is closed. `join` owns enrollment plus initial placement compensation; there is no public `nodes.enroll` twin. `leave` owns self-revocation/cache retirement after authority-transfer checks. Status/list/get/candidates and operation-specific `plan` are read/preflight shaped as cataloged; rotate/revoke/apply/run/pause/resume/repair/seed/verify/retire/promote/adopt/split are versioned/idempotent effects or resumable operations.

Plan 10's enrolled-node handshake/snapshot/tail/observation/ack protocol is a generated **internal protocol-binding facet**, not another application use-case family. It is excluded from agent tools, MCP resources/prompts, public CLI/help, dashboard actions, skills, hints, catalog search, and public SDK operation generation. Its rows bind only the authenticated node transport to the existing `brain.sync.*`, placement, membership, and replication application ports; they cannot be granted through a tool profile or invoked by generic catalog dispatch.

Plan 22 contributes this exact generated Context Scout family:

```text
scout.status.get
scout.runs.list
scout.runs.get
scout.envelopes.list
scout.envelopes.get
scout.decision.explain
scout.evaluation.get
scout.feedback.record
scout.runtime.pause
scout.runtime.resume
scout.runtime.cancel
```

These eleven canonical rows follow the same one-row rule. CLI presentation may call envelopes `suggestions`, but the semantic IDs remain `scout.envelopes.*`; HTTP uses `/api/v2/scout/suggestions`; no `scout.suggestions.*` alias exists. Read rows generate CLI, HTTP/SDK, dashboard, and reviewed compact MCP bindings. Feedback is an audited evidence append; pause/resume/cancel are typed system controls. Historical replay uses only `experiments.draft_from_selection`, `experiments.create`, and `experiment_runs.*` with `LabKindV1::Hint` plus scout evaluator mode, so catalog generation rejects every `scout.replay.*` row.

The exact host-integration family is likewise closed and generated:

```text
integrations.list|get|diff|status
integrations.install|update|repair|uninstall|verify
```

`list|get|diff|status` are admin-scoped reads over stored application views; only `verify` performs a fresh host probe, through a resumable operation. The five lifecycle commands require the administrative integration grant, expected desired/observed/manifest versions, and idempotency; they return the shared `OperationRef`. Skills plus the singular plan-21 `tracedecay integration` CLI tree provide the MCP-free workflow. If explicitly exposed through MCP, these nine bindings belong only to a reviewed `tracedecay-operator` profile—never `context`, `work`, a generic installer tool, or a host-specific server. Their schemas contain opaque target/installation/component/credential refs and safe state/digests only; host paths, config/backup bodies, command lines, environment values, credential values, and arbitrary manifests are structurally absent.

`saved_views.*` is the shared plan-09/11 lifecycle for `SavedViewDefinitionV1::{Investigation,Task,Experiment}`; plan 24 supplies task-variant validation and lenses but no `task_views.*` alias, route, or store. The experiment variant imports plan 01's exact experiment/run/cell/stage/comparison/comparison-cell/reduction/playhead identity set without embedding artifacts. `task_offers.accept` is the sole public command that may invoke atomic attempt/lease/start admission; no `work_items.acquire_lease` binding exists. `context_packets.accept` is the sole fenced command that may advance an attempt's monotonic accepted-packet pointer; the immutable start packet remains separately visible in attempt detail/timeline. Manual attestation/review/decision/exception/handoff/reopen/reversal remain distinct typed commands, not a generic status setter or rollback. Notification create/update/delete are direct validated commands and have no preview/apply aliases. Each row above has an explicit application, CLI, MCP, HTTP, dashboard, Rust, TypeScript, and Python binding or a reviewed audience-specific unavailability disposition.

The seven `task_graph.edit_bundles.*` operations are the sole bulk-edit semantics for a large plan/task graph. `export` freezes the authorized plan version and creates a bounded, protected, expiring structured-staging bundle whose canonical editable representation is versioned frontmatter Markdown; `get` reads its metadata/content under current authorization; `validate` returns parse/schema/reference/graph/policy/privacy diagnostics; `diff` returns the typed semantic delta; `rebase` creates a successor bundle over a newer base with explicit conflicts; `submit` recompiles and revalidates the exact document digest before one owner-shard expected-version transaction; `delete` idempotently retires staged payload and schedules safe cleanup. These operations reuse the application operation/structured-staging kernel and canonical task commands; the file is never task truth, omission never implies deletion, and lease/fence/attempt/derived-status fields are unrepresentable. Only `tracedecay-work`'s `orchestrator` profile exposes the mutating `export`, `rebase`, `submit`, and `delete` bindings; `task-worker` retains only its addressed task/attempt lifecycle and bounded assigned-slice reads, and no context profile exposes a task-graph mutation.

Autonomous-loop admission is observable through one closed read family generated from the same application views as every other surface:

```text
automation.dirty_scopes.list
automation.admissions.list
automation.admissions.get
```

`automation.dirty_scopes.list` returns the exact `AutomationWorkKeyV1` and `AutomationScopeCursorV1`, the per-shard current, considered, consumed, and included frontiers, pending delta, unconsumed dirty generation/count/reasons, quiet and retry deadlines, last-terminal semantic input/outcome, active-writer/coverage state, and the shared policy/operation health references for retry, circuit, pause, quarantine, reconciliation, and incomplete coverage. `automation.admissions.list` has one generated representation selector: exact receipt rows or bounded coalesced skip episodes. An episode preserves its stable anchor, first/last evaluation times, evaluation count, latest policy-evaluation ID, job/scope, exact reason, semantic-input/frontier pair, next reconsideration, and model/tool/token/cost work avoided; it is a projection/query aggregation, not a run, admission receipt, or fourth operation. `automation.admissions.get` returns the exact domain `AutomationAdmissionReceiptV1`. These reads never expose eligible payload bytes, protected dependency-manifest content, secret-derived identifiers, or protected quarantine content.

All three operations generate one catalog-owned cross-surface binding row with a CLI command, MCP tool, HTTP route/SDK method, dashboard action, and canonical JSON presentation; `automation.admissions.get` additionally declares an addressable read-only MCP resource template on that same use case. MCP resource discovery never substitutes for either semantic list operation. Existing `automation.jobs.*` and `automation.scheduler.status` views reuse the same generic operation, retry directive, policy-health, circuit, pause, quarantine, and coverage types; this family does not create transport-local state enums. The existing autonomous `run_now` command remains a normal admission request: it may shorten cadence for a dirty scope but cannot bypass `IdenticalTerminalInput`, successful/`NoChange` terminal fencing, privacy/quarantine policy, or retry/circuit state. Unchanged and historical evaluation belongs to a generic experiment.

Plan [`15-search-quality-evaluation-and-retrieval-research.md`](15-search-quality-evaluation-and-retrieval-research.md) contributes one closed search-evaluation family. These operation IDs are canonical and exhaustive for this slice:

```text
reads:
retrieval.corpus_versions.list|get
retrieval.qrel_versions.list|get
retrieval.candidate_pools.list|get
retrieval.judgments.list|get
retrieval.adjudications.list|get
retrieval.evaluation_reports.list|get
retrieval.profiles.list|get
experiments.evaluator_catalog.get
experiments.draft_from_selection
experiments.list|get
experiment_runs.list|get
experiment_cells.list|get
replay_stages.list|get
replay_comparisons.list|get
replay_comparison_cells.list|get
replay_reductions.list|get

commands:
retrieval.corpus_versions.create|freeze
retrieval.qrel_versions.create|freeze
retrieval.candidate_pools.create
retrieval.judgments.record|supersede
retrieval.adjudications.record
experiments.create
experiment_runs.create|cancel|resume|retry|minimize
retrieval.evaluation_reports.publish
experiments.fixtures.promote
retrieval.profiles.publish|activate
```

`replay_stages.list` requires one `cell_id` and returns the cursor window as exact domain `ReplayTraceV1` (run, cell, ordered stage window, continuation, total, terminal receipt, coverage); `replay_stages.get` returns one `ReplayStageV1`. This is the shared trace read for every lab, not a hidden UI assembly or extra operation.

Every read generates catalog, CLI, MCP-tool, MCP-resource/resource-template, HTTP/SDK, and Search Quality UI parity metadata where the surface supports reads; each `get` resource URI resolves the same application view and each `list` remains the semantic list operation rather than abusing MCP `resources/list`. Commands generate CLI/MCP/HTTP/SDK/UI bindings only and never a writable resource. The catalog must not invent `eval`, `benchmark`, `golden`, any `retrieval.fixtures.*` alias, or other use case absent from the family above; a surface omission requires an explicit reviewed disposition.

The generic experiment operations above are shared with every `LabKindV1`; Search Quality selects its tagged evaluator/corpus schema and does not mint `retrieval.evaluation_runs.*` aliases. Reports/profile promotion remain retrieval-owned because they change the published retrieval product, whereas experiment execution remains hermetic and product-read-only. Sanitized evaluator-fixture promotion—including Search Quality fixtures—is the one typed `experiments.fixtures.promote` command; no retrieval alias exists.

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
- [`05-query-crate.md`](05-query-crate.md) §11.2A/PR 14E owns optional representation-model artifact semantics and lifecycle delivery. This catalog declares the generated `representations.artifacts.*` and `representations.generations.*` capabilities, effects, config dependencies, availability, and typed views; it never downloads, verifies, loads, or evicts an artifact.
- Optional native semantic code search has exactly one epoch-one implementation disposition: FastEmbed with the benchmark-promoted embedding artifact (`JinaEmbeddingsV2BaseCode` is the primary candidate and `GTELargeENV15Q` the required comparator) and, when independently promoted, a `BGERerankerV2M3` artifact. It reuses `search.universal`, `code.search_symbols`, `representations.artifacts.*`, and `representations.generations.*`; it creates no provider-specific use case, alias, crate, MCP tool, or hidden download capability. Before benchmark promotion the semantic contribution is disabled by default and the lexical result is authoritative.
- The generated availability contract exposes separate `desired`, `activated`, `effective`, and `observed` enablement plus exact artifact/model/runtime/generation references, rebuild coverage, and typed unavailable/degraded/error reasons. `desired` is configuration intent, `activated` is a verified artifact/generation selection, `effective` is the policy/budget-compatible route for the request, and `observed` is what the daemon actually loaded/executed. No layer infers another and no unavailable artifact selects an alternate model.
- A second, separately registered rerank route may use a discovered Codex Spark/app-server-style capability. It is optional, off by default, never supplies embeddings, and cannot replace the native FastEmbed embedding plus optional BGE rerank route. Its catalog entry declares discovery evidence, credential-reference/egress/privacy requirements, model and cost/token/deadline/candidate budgets, requested-versus-actual route receipts, and typed unavailable/timeout fallback that returns the byte-stable pre-rerank order. It links to plan 22's active hinting/scout model-routing contracts but adds no scout operation or implicit agent-model entitlement.
- PR #410's direct_user/subagent/tool_result filters and parent-representative dedupe are semantic query capabilities, not presentation-only toggles.
- Git output distinguishes directly_changed, structurally_impacted, candidate_test, and context_only. Transitive/file-level graph fanout can never be labeled direct modification.

## 2. Goals

- Inventory every current MCP tool, top-level and recursive CLI command, HTTP method/route, dashboard plugin/action, managed/bundled skill, hook event/effect, config mutation, background operation, and compatibility alias.
- Assign stable CapabilityId, UseCaseId, IntentId, BindingId, semantic version, owner, lifecycle, and replacement to each.
- Require domain `ScopeSelectorV2` on every scoped capability/binding; catalog metadata can constrain allowed scope kinds but cannot invent a transport-specific/current-project selector.
- Generate MCP schemas/descriptions/categories, CLI command metadata/help cross-references, HTTP/OpenAPI operation metadata, dashboard command/panel manifests, skill/tool references, hook routing facts, documentation tables, and TypeScript catalog types from one definition set.
- Generate MCP tools, resources, resource templates, prompts, completion eligibility, output schemas, effect annotations, task-support declarations, subscription/list-change facts, and protocol-capability requirements from one reviewed binding set; the live adapter may not hand-register an MCP primitive.
- Generate the three logical MCP registrations and their immutable exposure profiles from explicit binding sets, including component-set/install-scope/profile selection, host/effect/grant intersections, eager-host budgets, and tools-only fallbacks; never generate a profile from a domain/name glob.
- Generate the exact `task_graph.edit_bundles.export|get|validate|diff|rebase|submit|delete` bindings and their frontmatter-Markdown/resource-link/output contracts without inventing a generic document mutation or transport-local bulk editor.
- Generate the exact `integrations.list|get|diff|status|install|update|repair|uninstall|verify` family, singular CLI tree, admin HTTP/SDK/dashboard bindings, operator-only optional MCP projection, shared operation lifecycle, and path/content-free sealed views without inventing provider-specific installers or transport-local status.
- Generate exact search-evaluation CLI/MCP/resource/UI parity rows from the closed `retrieval.*` operation family above; reject invented aliases, missing reads/commands, and resource bindings that mutate evaluation state.
- Generate exact automation dirty-scope/admission parity rows, including receipt-versus-coalesced-episode representation and per-shard current/considered/consumed/included frontiers, without minting a skip-episode operation or transport-local retry/circuit/quarantine/reconciliation schema.
- Make read/mutate, manual-versus-autonomous execution, side effects, idempotency, dry-run/preview, confirmation, automatic recovery/compensation, streaming/pagination, cost, latency, freshness, security, privacy, and audit behavior explicit. Curation item effects are autonomous and have no approval/apply binding.
- Give policy compact, versioned task-to-capability facts without shipping the entire catalog in every hint.
- Catalog current agent-presence/work-claim publish, heartbeat, nearby-query, overlap acknowledgement/handoff, coordination analytics, and Coordination Lab capabilities with advisory/privacy/TTL/trigger semantics.
- Route Git intent to branch_list, branch_search, branch_diff, pr_context, changelog, commit_context, sessions_for, and workflows with exact local/live/joined truth requirements.
- Reconcile local semantic Git state and live GitHub/delivery state by ref/merge-base/head/changed-file universe/fetched-at/index watermark.
- Detect catalog drift in CI whenever a surface is added, removed, renamed, or semantically changed without a catalog/version/parity disposition.
- Preserve V1 behavior as differential/import evidence until each cutover, but publish no old runtime tool names, aliases, response-handle quirks, or stale client schemas afterward. Current capability metadata is authoritative.
- Make missed capability, fallback, user correction, unavailable capability, and useful silence observable outcomes.

## 3. Non-Goals

- No use-case execution, application orchestration, storage/query/policy logic, provider API call, Git/GitHub call, filesystem scan at runtime, dashboard rendering, MCP transport/lifecycle/session state, CLI parser, or Axum router.
- No TraceDecay runtime plugin marketplace, arbitrary executable extension feed, or remote semantic catalog service in the first V2 default. This does not prohibit deterministic plan-27 publication of signed generated TraceDecay packages into Codex/Claude/Cursor native marketplaces; those packages are release artifacts from this catalog, not runtime-loaded semantic authorities.
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
- Merged PR #414 adds the `move_symbol` edit capability; regenerate the tool/CLI/API inventory and require owner/schema/scope/effect/idempotency/inspect/commit/error bindings rather than treating the old 102-name count as current.
- Merged release PRs #413/#416/#418 and merged #407/#415/#417/#419/#420/#422/#423/#424/#425 remain historical inputs; PR #409 remains closed historical inventory only. The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). Untracked branch-graph recovery, divergent-session preservation, bounded consolidation lookup, lifecycle-lease-safe hooks, conflict-safe registry reconstruction, read-only FTS search, graph peer-checkpoint safety, and proof-gated retirement of applied consolidation manifests remain required catalog fixtures.

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

The tool CLI binding `tracedecay tool <current-name>` is recorded separately from native CLI commands because it has MCP-argument parity, `--args`/`--dry-run`/`--json`/`--project` behavior, response handles, and a different compatibility contract. V2 keeps it as the schema-exact CLI fallback for skills and MCP-optional hosts, generated from the same current public binding rows and subject to identical effects, grants, scope, authorization, and output contracts. It cannot accept an arbitrary hidden `BindingId`, bypass availability, or become an MCP tool; it is therefore not the forbidden generic invoke/god surface.

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

- Codex: exact current set `SessionStart`, `SubagentStart`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `SubagentStop`, and `Stop`; no invented `PostToolUseFailure`;
- Claude Code: independent current 30-event manifest for `SessionStart`, `Setup`, `InstructionsLoaded`, `UserPromptSubmit`, `UserPromptExpansion`, `MessageDisplay`, `PreToolUse`, `PermissionRequest`, `PermissionDenied`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Notification`, `SubagentStart`, `SubagentStop`, `TaskCreated`, `TaskCompleted`, `Stop`, `StopFailure`, `TeammateIdle`, `ConfigChange`, `CwdChanged`, `FileChanged`, `WorktreeCreate`, `WorktreeRemove`, `PreCompact`, `PostCompact`, `Elicitation`, `ElicitationResult`, and `SessionEnd`;
- Cursor: before_submit_prompt, subagent_start, post_tool_use, session_start, session_end, stop, pre_compact, after_file_edit, after_shell, workspace_open;
- Kiro: pre_tool_use, prompt_submit, post_tool_use;
- MCP/daemon hook events: FileEdit, Shell, WorkspaceOpen, SessionStart, IncrementalSync;
- shared effects: capture, inject context/hint, allow/deny, reset/accounting marker, transcript catch-up, LCM lifecycle, file/project sync, branch/worktree tracking, analytics/outcome evidence.

The hook facet stores one closed `HostHookBindingSpecV1` per host-version/event with its release-manifest-bound `HostHookBindingId`. It declares canonical and native event IDs, invocation scope/cadence, exact matcher compiler/subject/aliases (or ignored matcher), handler-level filter semantics, required/nullable common and event fields, allowed handler kinds/execution modes, timeout/default override, platform exec/shell lowering, allowed stdout/HTTP/MCP/prompt-agent/JSON/exit result shapes, legal blocking/rewrite/permission/continuation/display/worktree/elicitation effects, durability/privacy class, interception coverage denominator, capability disposition/evidence, and conformance case IDs. Codex consumes an independent ten-event required manifest. Claude consumes an independent version-pinned 30-event manifest whose oracle is not generated output and records `Supported|VersionGated|Absent|PolicyDisabled` for every event × handler type × surface. Event-response legality and handler support are catalog data consumed unchanged by plans 07/21/27, never adapter-local match logic.

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
│   ├── effect.rs                      # read/mutate/idempotency/confirmation/recovery metadata
│   ├── availability.rs                # prerequisites/capability gaps
│   ├── freshness.rs                   # local/live/joined truth requirements
│   ├── privacy.rs                     # sensitivity/access/audit declarations
│   ├── lifecycle.rs                   # active/deprecated/replaced/legacy/pending
│   ├── registry.rs                    # validated immutable built-in registry
│   ├── snapshot.rs                    # catalog field declarations; invokes domain CanonicalEncode/digest kernel
│   ├── mcp_profile.rs                 # logical registrations, immutable exposure profiles, component sets, budgets
│   ├── host_integration/
│   │   ├── mod.rs
│   │   ├── manifest.rs                # one canonical HostIntegrationManifestV1 source IR
│   │   ├── bundle_projection.rs       # skills/roles/hooks/packages projection refs
│   │   ├── capability.rs              # documented/validated/assumed capability evidence
│   │   ├── differences.rs             # typed cross-host capability differences
│   │   ├── install_set.rs             # HostInstallSetV1 validation
│   │   └── validation.rs
│   ├── host_bundles/
│   │   ├── mod.rs
│   │   ├── source.rs
│   │   ├── compiler.rs                # pure deterministic lowering; no host I/O
│   │   ├── artifact.rs
│   │   ├── deterministic.rs
│   │   ├── validation.rs
│   │   ├── diagnostics.rs
│   │   ├── conformance.rs
│   │   └── hosts/{mod,claude,codex,cursor}.rs
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
│   │   ├── task_graph.rs
│   │   ├── integrations.rs
│   │   ├── observability.rs
│   │   ├── operations.rs
│   │   └── experiments.rs
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
│   ├── mcp-protocol.json
│   ├── mcp-surface-profiles.json
│   ├── mcp-tools.json
│   ├── mcp-resources.json
│   ├── mcp-prompts.json
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
│   ├── host-integration-manifest-v1.json
│   ├── host-capability-registry-v1.json
│   ├── host-bundle-payload-v1.json
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
│   ├── mcp_protocol_generation.rs
│   ├── mcp_surface_profiles.rs
│   ├── task_graph_edit_bundle_bindings.rs
│   ├── host_integration_bindings.rs
│   ├── host_bundle_generation.rs
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
src/v2/hooks/conformance/manifest.rs
crates/tracedecay-application/src/registry.rs
contracts/api/openapi/generated.json
dashboard/app/src/generated/{catalog.ts,commands.ts}
src/mcp/generated/{protocol.rs,tools.rs,resources.rs,prompts.rs}
src/cli/generated_v2.rs
docs/reference/generated-capabilities.md
~~~

Generated files carry a source digest header and are never hand-edited. The public OpenAPI, JSON Schema, and SDK trees are produced through plan 17's contract-IR pipeline (plan 17 §5.1): this catalog contributes `openapi-operations.json` operation metadata to the IR, and plan 17 owns generation of `contracts/api/openapi/generated.json` and the client packages.

### 6.1 Catalog-owned unsigned host-bundle contracts

The catalog is the sole owner of the pure compiler's unsigned payload, artifact, and compile-result types. They contain deterministic release inputs only; a runtime capability probe, scan/conformance receipt, release attestation, signature, marketplace locator, host instance, operation, clock, workspace root, or deployment state cannot enter them.

```rust
pub struct GeneratedHostArtifactV1 {
    pub package_id: RegistryEntryId,
    pub relative_path: SafeRelativePath,
    pub media_type: MediaTypeCode,
    pub file_mode: PortableFileModeV1,
    pub content_digest: ContentDigest,
    pub source_components: BTreeSet<HostComponentRefV1>,
    pub contains_executable_code: bool,
    pub sensitivity: DataSensitivity,
}

pub struct HostBundlePayloadV1 {
    pub schema_version: SchemaVersion,
    pub bundle_id: ManifestId,
    pub bundle_version: ComponentVersion,
    pub host_profile: HostProfileRef,
    pub package_id: RegistryEntryId,
    pub source_commit: ContentDigest,
    pub integration_manifest: ManifestDigest,
    pub catalog: CatalogSnapshotRefV1,
    pub policy_bundle: PolicyBundleRef,
    pub config_schema_digest: RegistryManifestDigest,
    pub sanitizer_floor: SanitizerFloorId,
    pub privacy_policy: PrivacyPolicyDigest,
    pub adapter_version: ComponentVersion,
    pub binary_compatibility: BinaryCompatibilityRequirementV1,
    pub stock_capability_evidence_manifest: ManifestDigest,
    pub artifacts: BoundedVec<GeneratedHostArtifactV1, 8192>,
    pub omissions: BoundedVec<HostComponentOmissionV1, 1024>,
    pub difference_ledger: ManifestDigest,
    pub conformance_input_manifest: ManifestDigest,
    pub provenance_input_manifest: ManifestDigest,
    pub sbom_input_manifest: ManifestDigest,
    pub license_input_manifest: ManifestDigest,
    pub release_scan_input_manifest: ManifestDigest,
}

pub struct HostBundleCompileResultV1 {
    pub input_digest: ManifestDigest,
    pub payload: HostBundlePayloadV1,
    pub payload_digest: ManifestDigest,
    pub validation_report: ManifestDigest,
    pub deterministic_rebuild_input_manifest: ManifestDigest,
}
```

`payload_digest` covers canonical `HostBundlePayloadV1` bytes only. The payload does not contain its own digest. Its stock capability evidence is the sanitized, versioned plan-13 release input selected before compilation, never a current-host observation. PR 36R consumes these inputs and is the only owner allowed to independently rebuild, scan, run stock-host conformance, attest, sign, or publish.

## 7. Dependency Direction and Forbidden Imports

~~~text
tracedecay-domain
        ↑
tracedecay-tool-catalog
        ├──→ tracedecay-policy
        ├──→ tracedecay-application ──→ CLI/MCP/API/dashboard adapters
        └──→ root::v2::hooks
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
- `capability.experiments.replay`;
- `usecase.experiments.draft-from-selection`;
- `usecase.experiments.create`;
- `usecase.experiment-runs.create`;
- `usecase.experiment-runs.cancel` / `resume` / `retry` / `minimize`.
- `usecase.experiments.promote-fixture` (separate confirmed, secret-scanned command outside evaluator runtime).
- `usecase.automation.list-dirty-scopes`;
- `usecase.automation.list-admissions`;
- `usecase.automation.get-admission`.

The coordination playground is the `LabKindV1::Coordination` evaluator entry under that generic experiment capability, not a second run/status/cancel use case. Experiment catalog entries declare accepted source-selection kinds, tagged input/parameter/stage/output schemas, immutable manifest requirements, default/hard resource and egress caps, exact/recorded/best-effort support, removable minimization dimensions, dashboard route, CLI/MCP/API bindings, and the absence of production effect ports. The broader coordination entries declare profile/activity ownership, <=160-character safe-summary schema, retrieval-anchor privacy, heartbeat/TTL/status, repository/worktree/ref/PR/file/symbol/query scopes, read/write intent, redundancy modes, cursor/cap semantics, and effect owner. `find-nearby-work` is bounded to 100 and read-only. Claim/ack mutations are idempotent/audited. The catalog never grants cancellation/reassignment/lock/message authority.

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
    pub budget: SurfaceBudgetV1,
    pub mcp: Option<McpBindingSpecV1>,
    pub availability_override: Option<AvailabilitySpec>,
    pub compatibility: CompatibilityDisposition,
}

pub struct SurfaceBudgetV1 {
    pub max_page_items: u32,
    pub max_inline_bytes: u32,
    pub max_inline_tokens: u32,
    pub max_total_output_bytes: u64,
    pub overflow: SurfaceOverflowPolicyV1,
}

pub enum SurfaceOverflowPolicyV1 {
    Cursor,
    ResourceLink,
    OperationArtifact,
    Reject,
}

pub struct McpProtocolRevision(CatalogText); // YYYY-MM-DD stable MCP revision

pub struct McpBindingSpecV1 {
    pub primitive: McpPrimitiveV1,
    pub protocol_revisions: BTreeSet<McpProtocolRevision>,
    pub input_schema: Option<SchemaRef>,
    pub output_schema: Option<SchemaRef>,
    pub annotations: Option<McpToolAnnotationsV1>,
    pub content_annotations: Option<McpContentAnnotationsV1>,
    pub task_support: McpTaskSupportV1,
    pub completion: McpCompletionEligibilityV1,
    pub subscription: McpSubscriptionSpecV1,
    pub list_generation: McpListGenerationKind,
}

pub enum McpPrimitiveV1 {
    Tool,
    Resource,
    ResourceTemplate,
    Prompt,
}

pub struct McpToolAnnotationsV1 {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

pub struct McpContentAnnotationsV1 {
    pub audience: BTreeSet<McpAudienceV1>,
    pub priority_millis: Option<u16>, // generated as MCP 0.0..=1.0
}

pub enum McpAudienceV1 { User, Assistant }

pub enum McpTaskSupportV1 { Forbidden, Optional, Required }
pub enum McpCompletionEligibilityV1 { None, PromptArguments, ResourceTemplateArguments }
pub enum McpSubscriptionSpecV1 { NotSubscribable, Immutable, NotifyOnChange }
pub enum McpListGenerationKind { Tools, Resources, Prompts }

pub use tracedecay_domain::{McpLogicalRegistrationId, McpSurfaceProfileId};

pub struct McpSurfaceProfileV1 {
    pub id: McpSurfaceProfileId,
    pub version: Version,
    pub registration: McpLogicalRegistrationId,
    pub audience: McpProfileAudienceV1,
    pub bindings: BTreeSet<BindingId>, // exact, fully materialized set; no patterns
    pub required_host_features: BTreeSet<McpHostFeatureV1>,
    pub allowed_execution_modes: BTreeSet<ExecutionModeV2>,
    pub required_grants: BTreeSet<CapabilityGrantId>,
    pub grant_ceiling: BTreeSet<CapabilityGrantId>,
    pub max_tools: u16,
    pub max_definition_tokens: u32,
    pub max_definition_bytes: u32,
    pub max_input_schema_bytes: u32,
    pub max_description_bytes: u16,
    pub definition_token_estimator: AlgorithmRef,
    pub tools_only_fallbacks: BTreeMap<BindingId, BindingId>,
    pub definition_digest: ManifestDigest,
}

pub struct McpLogicalRegistrationV1 {
    pub id: McpLogicalRegistrationId, // tracedecay-context | tracedecay-work | tracedecay-operator
    pub profiles: BTreeSet<McpSurfaceProfileId>,
    pub explicit_opt_in: bool,
}

pub enum HostInstallComponentKindV1 {
    CoreSkillsCli,
    McpFacade {
        registration: McpLogicalRegistrationId,
        profile: McpSurfaceProfileId,
    },
}

pub struct HostInstallSetV1 {
    pub host_profile: HostProfileRef,
    pub components: BoundedVec<HostInstallComponentKindV1, 4>,
    pub integration_manifest_digest: ManifestDigest,
    pub component_set_digest: ManifestDigest,
}

pub enum McpProfileAudienceV1 {
    AgentCore,
    DeveloperRead,
    ResearchRead,
    TaskWorker,
    Orchestrator,
    Operator,
    AdminLab,
}

pub enum McpHostFeatureV1 {
    Tools,
    StructuredContent,
    ResourceLinks,
    Resources,
    ResourceTemplates,
    Prompts,
    Completion,
    ListChanged,
    Progress,
    Cancellation,
    ProtocolTasks,
    Sampling,
    Elicitation,
}

pub use tracedecay_domain::SurfaceKind;

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

pub struct LabEvaluatorDefinitionV1 {
    pub lab: LabKindV1,
    pub accepted_selection_kinds: BTreeSet<RegistryEntryId>,
    pub input_schema: SchemaRef,
    pub parameter_patch_schema: SchemaRef,
    pub stage_schema: SchemaRef,
    pub output_schema: SchemaRef,
    pub removable_dimensions: BTreeSet<RegistryEntryId>,
    pub replay_modes: BTreeSet<ReplayMode>,
    pub default_budget: ExperimentBudgetV1,
    pub required_capabilities: BTreeSet<CapabilityId>,
    pub dashboard_route: SurfaceInvocationCode,
}

pub enum QueryPresetScopeModeV1 { ActiveProfileAll, ExplicitScopeRequired }

pub struct QueryPresetDefinitionV1 {
    pub preset_id: RegistryEntryId,
    pub version: ComponentVersion,
    pub scope_mode: QueryPresetScopeModeV1,
    pub entity_kinds: BTreeSet<EntityKind>,
    pub event_kinds: BTreeSet<EventKind>,
    pub predicates: BTreeSet<PredicateId>,
    pub default_temporal: TemporalClauseV1,
    pub default_sort: BoundedVec<SortKey, 8>,
    pub exposed_facets: BTreeSet<AttrKeyId>,
    pub allowed_views: BTreeSet<RegistryEntryId>,
    pub required_capabilities: BTreeSet<CapabilityId>,
    pub definition_digest: ManifestDigest,
}

pub struct VisualSemanticDefinitionV1 {
    pub semantic_id: RegistryEntryId,
    pub subject_class: VisualSubjectClassV1, // entity, relation, lane, interval, metric, aggregate, or state
    pub subject_kind: RegistryEntryId,
    pub family_glyph: RegistryEntryId,
    pub icon_asset: RegistryEntryId,
    pub scope_contour: RegistryEntryId,
    pub evidence_treatment: RegistryEntryId,
    pub relation_stroke_and_head: RegistryEntryId,
    pub chart_mark: Option<RegistryEntryId>,
    pub temporal_freshness_treatment: RegistryEntryId,
    pub coverage_privacy_texture: RegistryEntryId,
    pub focus_treatment: RegistryEntryId,
    pub selection_treatment: RegistryEntryId,
    pub comparison_treatments: BoundedVec<RegistryEntryId, 6>,
    pub label_priority: u16,
    pub lod_representations: BTreeMap<RegistryEntryId, RegistryEntryId>,
    pub accessibility_label_template: CatalogText,
}

pub enum VisualSubjectClassV1 { Entity, Relation, Lane, Interval, Metric, Aggregate, State }

pub struct WorkspaceSlotDefinitionV1 {
    pub slot_id: RegistryEntryId,
    pub allowed_artifacts: BTreeSet<RegistryEntryId>,
    pub allowed_renderers: BTreeSet<RegistryEntryId>,
    pub allowed_docks: BTreeSet<RegistryEntryId>,
    pub required: bool,
}

pub struct WorkspaceCompositionDefinitionV1 {
    pub composition_id: RegistryEntryId, // exactly Atlas, Trace, Compare, Lab, or Triage in V2 epoch 1
    pub version: ComponentVersion,
    pub slots: BoundedVec<WorkspaceSlotDefinitionV1, 4>,
    pub allowed_layouts: BTreeSet<RegistryEntryId>,
    pub selection_schema: SchemaRef,
    pub legend_semantic_ids: BTreeSet<RegistryEntryId>,
    pub accessibility_order: BoundedVec<RegistryEntryId, 4>,
}
~~~

The initial generated preset registry includes `preset.knowledge.all-memories`, `preset.skills.all`, and `preset.automation.all`. They instantiate ordinary `TraceQueryV1` with an explicit active-profile All or caller-supplied scope; they are not stored result sets, endpoints, client filters, or another query AST. `all-memories` exhaustively covers fact/fact-version/knowledge-entity/knowledge-version/decision/contradiction/retrieval/feedback plus their source/use/outcome/revision/recovery relations; `skills.all` covers skill/package/version/materialization/use/outcome; `automation.all` covers job/admission/skip-episode/run/artifact/candidate/decision/effect/recovery. A registry-generation exhaustive match forces every newly added domain entity/event/predicate in those semantic families to be included or explicitly excluded with rationale. CLI, MCP, HTTP, SDK, Explorer, Brain, accessibility, and export consume the same preset ID/digest and then apply ordinary typed filters.

`SurfaceKind` is the one closed, generated surface vocabulary for binding identity and usage accounting. Stable integer codes and `snake_case` wire names are emitted with the catalog snapshot; plans 21 and 26 consume those generated values and may not maintain SQL-, renderer-, or telemetry-local surface lists. A genuinely new surface requires a catalog-schema version, compatibility disposition, accounting classification, and conformance fixtures before any binding can use it.

`McpBindingSpecV1` exists only when `surface == SurfaceKind::Mcp`. It classifies the binding by the MCP interaction model: model-controlled operations are tools; application-controlled context is a resource or parameterized resource template; user-selected workflow recipes are prompts. Completion is legal only for prompt arguments and resource-template variables, never as a private tool-argument completion protocol. Tool annotations are generated from `EffectSpec`, idempotency, and egress metadata and remain advisory wire hints; authorization never trusts them. `task_support` describes MCP protocol task augmentation for a long-running invocation, not plan 24's canonical `WorkItemId` or `ExecutionAttemptId`.

`McpSurfaceProfileV1` is an exposure projection, not a capability definition or authorization system. The built-in epoch-one matrix is fixed and generator-tested:

| Logical registration | Profiles | Hard profile ceilings | Effect ceiling |
|---|---|---|---|
| `tracedecay-context` | `agent-core`, `developer`, `research` | respectively 12/8k, 32/24k, and 24/18k tools/definition tokens | `ReadOnly` only |
| `tracedecay-work` | `task-worker`, `orchestrator` | respectively 24/16k and 32/24k | task-scoped `ReadOnly`, `DirectCommit`, and cataloged `ResumableWorkflow`; no operator-destructive effect |
| `tracedecay-operator` | `operator`, `admin-lab` | respectively 24/18k and 32/24k | exact explicitly granted operator/lab modes; registration is never installed implicitly |

Each `bindings` set is reviewed as concrete IDs in `mcp-surface-profiles.json`; adding a future binding to a capability family does not add it to any profile. `tools_only_fallbacks` maps a resource/prompt-assisted interaction to another existing binding of the same `UseCaseId` for an eager tools-only host and cannot create a generic dispatcher. Profile validation recomputes tool count, definition tokens, and serialized definition bytes with the pinned estimator after host fallback projection. Every epoch-one profile also respects the registration hard caps of 32 tools, 24,000 estimated definition tokens, 128 KiB serialized definitions, 8 KiB per input schema, and 512 UTF-8 bytes per description; a lower row-specific tool/token ceiling still wins. Thus a tools-only form cannot exceed a profile budget or change count semantics between releases silently. A host installer can choose one allowed profile or omit MCP entirely; registering two logical names never creates another adapter, catalog snapshot, daemon, application handler, or presentation path.

`SurfaceBudgetV1` is the only per-binding inline/page/output budget contract. Large eligible results follow the declared cursor, MCP resource-link, or contained operation-artifact overflow path; handlers and profiles do not invent an “inline byte budget.” Plan 21 imports this type unchanged for rendering/transport enforcement, while plan 20 exposes its generated values as immutable integrity state rather than writable settings.

`HostIntegrationManifestV1` imports one validated nonempty `HostInstallSetV1` and the allowed registration/profile pairs. Empty means no desired deployment and is represented by the absence/retirement of desired state through `integrations.uninstall`, never by an empty set. The set contains at most one core component and at most one facade per logical registration; duplicate registrations, incompatible profiles, implicit operator, or more than four components fail validation. Core plus any supported facade subset and an explicit headless facade-only set are representable without another workflow implementation. Installation records every exact component/profile digest beside the catalog/protocol digest and validates the set again at initialize. A broader principal still cannot exceed any profile's `grant_ceiling`; a narrower principal or current authorization may remove bindings. A profile or component-set change requires reconfiguration plus a new MCP connection. `listChanged` can shrink/refresh the authorized set within a pinned profile but cannot switch profiles, add a component, bypass the ceilings, or serve as turn-by-turn tool discovery. `HostInstallModeV1` exists only in the bounded plan-12/27 legacy migration decoder and is absent from all current catalog/runtime/API schemas.

`ExecutionModeV2` lives in this crate's `effect.rs` and is the only closed effect-mode enum; plan 21 §11.1 consumes it for surface annotations and defines no surface-local variant. `SurfaceInvocationCode` carries the current canonical surface name or route only; V1 names live solely inside `CompatibilityDisposition` (field contract defined in plan 21 §17.1) and `CatalogAlias` provenance rows. `PresentationId` replaces any binding-local view reference; presentation descriptors themselves are plan 21's.

`CatalogAlias` is an intent/search/provenance label inside a snapshot, not a callable MCP/CLI/HTTP/hook binding name. Only `SurfaceBinding` can be invoked, and generation includes only bindings active in the current protocol epoch — `schema_version` plus the exact plan-01 `CatalogSnapshotRefV1` pinned in `ToolCatalogSnapshot` (Section 9).

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
- MCP binding without exactly one primitive contract, generated JSON Schema 2020-12 input/output mapping where applicable, compatible protocol revision, and matching capability declaration;
- completion on a tool, subscription on a mutable resource without an authorization-safe change source, tool task support without an application operation/cancellation/result contract, or an MCP task ID mapped to a plan-24 domain task ID;
- MCP tool annotations that disagree with effect/idempotency/egress metadata, or authorization/policy that relies on those advisory annotations;
- MCP registration/profile names outside the closed matrix; a profile containing a pattern, unknown/non-MCP binding, duplicate surface name, binding from another logical trust boundary, execution mode above its ceiling, or grant above its ceiling; a host fallback whose `UseCaseId` differs; any eager-host projection above `max_tools` or `max_definition_tokens`;
- an MCP binding reachable through a generic invoke/god tool, profile membership that can change per turn, or a `listChanged` path that activates another profile;
- a task-graph edit-bundle mutation outside `tracedecay-work`/`orchestrator`, a writable MCP resource, an implicit-delete document mode, or a bulk-edit binding that bypasses the shared operation/structured-staging and canonical task-command schemas;
- search-evaluation artifact binding outside plan 15 §0.1's closed `retrieval.*` family or execution binding outside the generic experiment family, missing CLI/MCP/resource/UI parity metadata, an MCP resource that mutates state, or any alias that creates a second operation identity;
- lab evaluator without the generic experiment operations, source-selection mapping, closed schemas, budgets/replay modes/removable dimensions, or one dashboard route; any evaluator-owned run/status/cancel binding;
- knowledge/skill/automation kind exposed by a first-class inventory without one exhaustive generated query-preset inclusion/exclusion disposition; preset scope, kinds, facets, sort, view, or digest differing across surfaces;
- entity/relation/lane/interval/metric/aggregate/state kind used by a visual surface without exactly one generated visual-semantic entry and accessibility label; an icon/mark/state/temporal/focus/selection/compare meaning outside that entry; a nonversioned composition/slot/layout; a composition other than the registered Atlas/Trace/Compare/Lab/Triage set; or any feature-local glyph/stroke/texture/legend meaning that conflicts with the snapshot;
- scoped binding without `ScopeSelectorV2`, or any current-project/CWD/first-match/base-checkout/current-graph fallback;
- current inventory item with no owner/parity disposition.

## 9. Immutable Catalog Snapshot and Runtime Resolution

~~~rust
pub struct ToolCatalogSnapshot {
    pub schema_version: CatalogSchemaVersion,
    pub catalog_version: Version,
    pub snapshot_ref: CatalogSnapshotRefV1,
    pub built_from_commit: CommitDigest,
    pub definitions: BTreeMap<CapabilityId, CapabilityDefinition>,
    pub use_cases: BTreeMap<UseCaseId, UseCaseDefinition>,
    pub bindings: BTreeMap<BindingId, SurfaceBinding>,
    pub mcp_registrations: BTreeMap<McpLogicalRegistrationId, McpLogicalRegistrationV1>,
    pub mcp_surface_profiles: BTreeMap<McpSurfaceProfileId, McpSurfaceProfileV1>,
    pub intent_routes: BTreeMap<IntentId, Vec<RouteCandidate>>,
    pub lab_evaluators: BTreeMap<LabKindV1, LabEvaluatorDefinitionV1>,
    pub query_presets: BTreeMap<RegistryEntryId, QueryPresetDefinitionV1>,
    pub visual_semantics: BTreeMap<RegistryEntryId, VisualSemanticDefinitionV1>,
    pub workspace_compositions: BTreeMap<RegistryEntryId, WorkspaceCompositionDefinitionV1>,
    pub source_manifests: Vec<InventoryManifestRef>,
    pub config_registry_digest: ConfigRegistryDigest,
}

pub struct AvailabilityContext {
    pub host_profile: Option<HostProfileRef>,
    pub host_surface: Option<HostSurfaceKindV1>,
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
    pub catalog_snapshot: CatalogSnapshotRefV1,
}
~~~

`CatalogSnapshotRefV1 { generation, digest }` is the sole capability-catalog identity and is owned by plan 01. Its generation is the monotonic per-daemon-generation counter negotiated in the MCP handshake exactly as master plan §2.6 (merged #422) and plans 12/21 describe: a daemon increments it whenever it activates a different snapshot digest, and each MCP session pins the pair unchanged. The session also pins one generated logical registration, `McpSurfaceProfileId`, profile definition digest, and immutable profile membership ceiling. Its initial effective visible set is the intersection with negotiated host support, grant ceiling, and authorization. During that connection the set may only shrink when authorization is revoked or a capability becomes unavailable; the adapter either emits the corresponding generated `list_changed` generation or terminates the stale session. Any widening, restored capability, catalog/profile/component change, or new grant requires a fresh connection. This crate emits list generations and capability requirements; plan 21's connection actor owns negotiation, narrowing refresh, and delivery. It advertises `tools.listChanged`, `resources.listChanged`/`subscribe`, or `prompts.listChanged` only when the corresponding generated primitive and notification implementation exist, and coalesces at most one `notifications/*/list_changed` event per primitive generation per session. A notification never selects a new profile or expands beyond its explicit binding set. A paginated list cursor pins one generation/profile pair across every page. A client holding a stale snapshot/profile fails closed with plan 17's typed `client_update_required`/`daemon_restart_required`/`capability_replaced` codes naming the current binding; it never receives a silently widened tool/resource/prompt set. `config_registry_digest` pins plan 20's registry manifest that this snapshot was built from.

The planning protocol baseline is the current stable MCP revision `2025-11-25`; PR 22A refreshes that revision from the official version registry and pins it in `mcp-protocol.json`. Draft revisions never enter a release artifact implicitly. The V2 live catalog generates no `2024-11-05` compatibility surface: incompatible clients receive the current supported revision during initialization and must reconnect/update before any application or store access.

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
4. Declare the catalog field order, then invoke plan 01's `CanonicalEncode` and manifest-digest builders; no catalog-local sorter/encoder/hasher exists.
5. Generate the MCP protocol manifest, the three logical registrations and explicit immutable surface profiles, tool/resource/resource-template/prompt definitions, completion/subscription/list-generation metadata, CLI binding metadata/help links, OpenAPI operation metadata, TypeScript types, dashboard commands, experiment evaluator/source-selection catalog, complete visual-semantic ontology/assets/legends and five versioned workspace compositions, hook bindings, compact policy facts, and docs.
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
- every MCP profile's exact binding set, registration, host fallback, effect/grant ceiling, eager-host tool count, definition-token count, and digest match `mcp-surface-profiles.json`; no generated family expansion or per-turn membership exists;
- every task edit bundle operation maps once to its typed application contract, and only the `orchestrator` profile contains edit-bundle mutations;
- every `LabKindV1` has one typed evaluator/source-selection mapping and only the generic experiment lifecycle; every visual entity/relation/lane/interval/metric/aggregate/state kind has one nonconflicting semantic/icon/mark/accessibility entry; exactly five versioned compositions have typed slot/layout IDs and deterministic unknown-version fallback, consumed identically by dashboard, accessibility outline, and export;
- MCP `inputSchema`/`outputSchema`, `structuredContent`, compact Markdown, tagged tool-error outcome, effect annotations, protocol task support, resource links, and primitive availability all reparse to the same typed use-case/view contracts.
- MCP tools/resources/prompts list pages bind one catalog generation; completion/subscription/list-change declarations have matching generated handlers and no undeclared notification path.

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
- a host/surface adds a handwritten installer, binding switch, schema fixture, or conformance suite that the shared `HostIntegrationManifestV1` and generated matrix can express; generation emits the provider/surface cases once and adapters contribute only irreducible host behavior;
- an installer registers an MCP name other than `tracedecay-context`, `tracedecay-work`, or explicitly opted-in `tracedecay-operator`; a profile is assembled by pattern/runtime category, changes without reconnect, exceeds its host/effect/grant/budget envelope, or has no skills+CLI fallback disposition;
- a route/command/tool is renamed without alias/deprecation;
- a mutation lacks effect metadata;
- generated files/docs differ;
- policy/hook/client embeds an unknown catalog digest;
- Git result omits membership/evidence/freshness/caps;
- #410 filters differ between surfaces;
- #405/#407 migration aliases appear as separate active capabilities/profiles;
- #409 (closed without merge, superseded by #413/#416) appears as required behavior.

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
- MCP resource URIs, prompt arguments, completion values, annotations, icons, descriptions, and names are catalog-safe metadata. Raw paths, bearer tokens, provider secrets, prompt/session payloads, confirmation material, or credential-bearing URLs cannot appear in a generated definition or list-change notification.
- MCP principal visibility is generated from grants but enforced by application/policy on every call, resource read, completion, subscription delivery, task poll, and retrieval-anchor resolution. Hidden unauthorized bindings never leak through list counts, completion candidates, or changed notifications.
- Task edit-bundle content is protected staged payload, never catalog metadata, completion text, a resource-list description, or a server filesystem path. Resource links carry only opaque authorized bundle IDs. Submit/delete authority is unavailable outside the pinned `tracedecay-work` `orchestrator` profile and is still reauthorized by application.

## 15. Performance and Quality Gates

- Build/validate/generate the full current catalog in <=2 s and <=256 MiB on the reference machine.
- Load/canonical-verify snapshot in <=25 ms p95; exact ID lookup <=50 microseconds p95; route resolution over one intent <=250 microseconds p95.
- Compact hint routing facts for one intent/category <=1 KiB by default and <=4 KiB hard cap.
- Coordination route facts include no summaries/agent IDs, fit <=512 bytes, expose only five allowed trigger classes, and cannot resolve to a cancellation/reassignment/message effect.
- 10,000 concurrent readers during 100 snapshot publications see one complete digest each; no mix.
- Generation is byte-identical across clean runs/platform path differences/time zones/map insertion orders.
- 100% of live inventory rows have owner/use-case/binding/lifecycle; zero unexplained drift.
- 100% of mutations have effect/idempotency/audit/execution-mode/confirmation-or-autonomy/recovery disposition; 0 curation candidates have per-item preview/approve/apply/reject/rollback bindings.
- 100% of automation dirty/admission reads have one generated catalog mapping per declared surface primitive, with the exact tool/resource multiplicity above; receipt/episode pages preserve cursor/frontier/coverage truth, and `run_now` has zero identical-input bypass binding.
- Every MCP profile stays within its generated tool/definition-token ceiling after eager tools-only fallback projection; role-corpus required binding coverage is 100%, unauthorized/out-of-profile exposure is zero, and the skills+CLI-only installation passes the same semantic fixtures.
- All seven task edit-bundle operations have one binding disposition per supported surface; only `orchestrator` exposes the mutating export/rebase/submit/delete bindings, large MCP results become typed resource links, and no profile contains a generic invoke binding.
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

- [ ] Add definitions for all project/code/graph/Git/session/LCM/memory/policy/automation/representation-artifact/observability/operation/lab surfaces and all 104 source MCP definitions with dispositions, including `ast_grep_search` and `move_symbol`; 103 are installed at 0.0.47.
- [ ] Add current V2 coordination definitions/bindings for presence, claim, heartbeat, nearby work, overlap acknowledgement/handoff, analytics, and Coordination Lab. Fixture-lock parent prefix `019f4906`, four PR #359 child agents, and Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d`; these are evidence anchors, never catalog text.
- [ ] Add the exact task-graph edit-bundle operation family, frontmatter-Markdown schemas, protected resource-link/read bindings, structured diagnostics/diff/receipt views, and the rule that only `tracedecay-work`/`orchestrator` exposes its mutations.
- [ ] Add all nine host-integration definitions with read-versus-probe/effect metadata, `HostInstallSetV1`, admin/operator exposure, operation/idempotency/ownership/trust/restart views, and recursive rejection of paths, config/backup bodies, command/environment/credential values, and arbitrary manifests.
- [ ] Add the exact Section 1 search-evaluation reads and commands with CLI/MCP/resource/HTTP/SDK/Search Quality UI parity dispositions. Reject all shorthand aliases and do not synthesize fixture reads or writable resources.
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

**Files:** src/generate/*.rs; generated/*; dashboard/app/src/generated/*; docs/reference/generated-capabilities.md; tests/{generation_determinism,transport_parity,mcp_protocol_generation}.rs.

- [ ] Add golden tests for MCP protocol/tool/resource/resource-template/prompt/completion/subscription outputs and CLI/OpenAPI/TypeScript/dashboard/hook/policy/docs outputs, then reparse every artifact for parity.
- [ ] Generate `mcp-surface-profiles.json` with the three logical registrations, seven explicit role profiles, component-set/install-scope/profile selection, fully materialized `BindingId` sets, tools-only fallbacks, effect/grant/host ceilings, eager-host counts/token budgets, and definition digests. Reject wildcards, implicit operator installation, per-turn switching, or a generic invoke tool.
- [ ] Add edit-bundle golden rows for export/get/validate/diff/rebase/submit/delete, large-result resource links, Markdown-default/JSON-explicit diagnostics, and profile visibility.
- [ ] Add one search-evaluation golden matrix asserting every canonical read/command appears exactly once on each supported surface, every `get` resource resolves the same typed view, and no unlisted alias/use case is emitted.
- [ ] Add one query-preset golden matrix asserting the three initial preset IDs expand to the exhaustive registered entity/event/predicate sets and identical scope/facet/sort/view/digest semantics across application, CLI, MCP, HTTP, SDK, Brain, Explorer, accessibility, and export.
- [ ] Run tests. Expected: fail before generators exist.
- [ ] Implement deterministic generation and source-digest headers.
- [ ] Run generator twice from clean output and compare hashes. Expected: byte-identical.
- [ ] Re-run tests. Expected: all generated requests/results/effects/errors map losslessly; every advertised MCP capability, list-change kind, task-support value, completion argument, and subscription has a generated adapter owner.
- [ ] Commit: feat(catalog): generate capability surfaces from one source.

### Commit 6: Wire current adapters, internal V1 differential harness, and drift enforcement

**Files:** src/mcp/generated/{protocol.rs,tools.rs,resources.rs,prompts.rs}; src/cli/generated_v2.rs; typed legacy route/action/hook registries; CI scripts/workflows; tests/complete_inventory.rs.

- [ ] Add CI tests that deliberately register one uncataloged tool/command/route/action/hook and assert a named failure.
- [ ] Make current surfaces consume generated descriptions/schema/metadata. MCP registration must contain no hand-written tool/resource/prompt name, schema, annotation, task-support, list-change, or completion allowlist. Keep V1 handlers reachable only from the internal differential/shadow harness and never from live dispatch after cutover.
- [ ] Replace the full-catalog MCP installer with a generated component set over the one adapter; prove `CoreSkillsCli` installs no MCP entry, zero/one/many facade companions compose without duplicate semantics, context/work registrations cannot cross trust boundaries, a headless facade-only set is explicit, and operator requires explicit opt-in.
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
- every installed host has migrated to skills+CLI only or one of the three generated logical registrations, and no legacy full-catalog/generic-dispatch registration remains;
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
- [ ] Run plan 21's MCP protocol-generation and official-SDK conformance fixtures. Expected: every generated primitive/schema/capability re-parses, no undeclared method or notification exists, and no hand-maintained live definition survives.
- [ ] Run MCP profile/installer/eager-host/deferred-host conformance. Expected: one adapter, exact profile digests and intersections, no per-turn `listChanged` widening, no generic invoke tool, and zero operator binding in an implicit install.
- [ ] Run task edit-bundle catalog parity. Expected: exactly seven operations, safe resource links for large bundles, typed Markdown/JSON diagnostics, orchestrator-only mutations, and no transport-local bulk-edit semantic.
- [ ] Run Git routing/truth/output regression corpus including #410. Expected: correct tool, separated truth, direct/impact/test/context membership, evidence/caps.
- [ ] Run #410 filter parity across CLI/MCP/generated HTTP/dashboard/export schemas. Expected: identical semantics and raw-row coverage.
- [ ] Run the closed search-evaluation family parity fixture. Expected: exact canonical operations, read-only MCP resources, complete CLI/MCP/HTTP/SDK/UI mappings, and zero invented aliases.
- [ ] Run benchmark/concurrent-publication/privacy/fuzz gates from Sections 14–15. Expected: all pass.
- [ ] cargo tree -p tracedecay-tool-catalog --edges normal and forbidden-import scan. Expected: no application/store/query/policy/hook/server/UI execution dependency.
- [ ] Run the placeholder scan using split regex atoms: rg -n 'TB[D]|TO[D]O|\bimplement lat[e]r\b|\bfill i[n]\b|\bappropriate erro[r]\b|\bsimilar to Tas[k]\b' docs/plans/tracedecay-v2/08-tool-catalog-crate.md. Expected: no matches.

## 19. Definition of Done

- Every current and newly merged capability has one stable owner/use case/version and explicit surface/lifecycle mapping; all 104 source MCP definitions carry dispositions (103 installed in the planning runtime at 0.0.47; 102 at the older frozen inventory). The current publication source is referenced through master §2.6/plan 13 and remains separate from those historical installed-runtime capability counts.
- MCP, CLI, HTTP, dashboard, skills, hooks, policy hints, generated docs, and clients share semantic schemas/effects/errors without copy drift.
- MCP is generated as a complete primitive surface—not a tool-name list: protocol profile, tools, output schemas, resources/templates, prompts, completion eligibility, annotations, task support, subscriptions, and list generations are catalog-owned while lifecycle/session/transport execution remains in the thin adapter.
- Skills plus CLI work without MCP. Optional MCP uses one thin `tracedecay` integration binary/adapter/catalog connected to private `tracedecayd`, and only the generated `tracedecay-context`, `tracedecay-work`, and explicitly opted-in `tracedecay-operator` registrations with immutable explicit profile sets, fixed connection digests, intersection enforcement, and eager-host budgets.
- The exact task edit-bundle family exports, reads, validates, diffs, rebases, submits, and deletes protected frontmatter-Markdown staging through shared application machinery; large MCP bundles use resource links and only the orchestrator profile exposes mutations.
- The canonical search-evaluation family has exact generated CLI/MCP/resource/HTTP/SDK/Search Quality UI parity; no transport invents an alias, fixture read, writable resource, or second semantic operation.
- Native task bindings include attempt list/get/timeline, registration-scoped offer list/get/accept/decline plus authorized revoke, packet list/get/fenced accept with start-versus-current pointer visibility, and direct notification list/get/create/update/delete across every supported surface; no family is hidden inside generic work-item detail or preview/apply aliases.
- The right TraceDecay Git capability is discoverable at the right intent, with live/local truth and output membership impossible to confuse.
- #405/#407 ownership, #410 filtering/dedupe, #411 remediation ownership, and #412 lifecycle prerequisites are cataloged; #413 contributes actual release/protocol version; #409 remains historical only.
- Missed capability and human correction are replayable evidence, while useful silence remains measurable.
- Presence/claim/nearby/ack/handoff/Coordination-Lab capabilities are current, bounded, privacy-safe, trigger-constrained, advisory, planned-redundancy-aware, and impossible to confuse with agent-control authority.
- Every scoped binding consumes the same `ScopeSelectorV2` plus pinned `ScopeResolutionV2`; multi-repo/project/checkout/worktree/ref/snapshot/generation selections and ambiguity/staleness remain visible and no surface invents a current-project/base-checkout/current-graph fallback.
- Catalog generation is deterministic, compact, privacy-safe, versioned, replayable, and enforced by CI.
- The catalog contains no business execution, storage, query, network, Git, host, or UI implementation.
