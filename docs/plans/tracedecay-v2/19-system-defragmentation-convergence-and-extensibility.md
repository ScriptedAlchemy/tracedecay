# TraceDecay V2 System Defragmentation, Convergence, and Extensibility Plan

**Status:** program-level implementation blueprint; this document changes no product code, data, store, or protocol.

**Parent plan:** [`../2026-07-09-tracedecay-brain-rewrite.md`](../2026-07-09-tracedecay-brain-rewrite.md)

**Normative supporting plans:** [`01-domain-crate.md`](01-domain-crate.md), [`02-store-crate.md`](02-store-crate.md), [`03-capture-crate.md`](03-capture-crate.md), [`04-projectors-crate.md`](04-projectors-crate.md), [`05-query-crate.md`](05-query-crate.md), [`06-policy-crate.md`](06-policy-crate.md), [`07-hooks-crate.md`](07-hooks-crate.md), [`08-tool-catalog-crate.md`](08-tool-catalog-crate.md), [`09-application-crate.md`](09-application-crate.md), [`10-api-crate.md`](10-api-crate.md), [`11-dashboard-frontend.md`](11-dashboard-frontend.md), [`12-root-compatibility-migration.md`](12-root-compatibility-migration.md), [`13-research-provenance-and-context-anchors.md`](13-research-provenance-and-context-anchors.md), [`14-historical-failure-regression-matrix.md`](14-historical-failure-regression-matrix.md), [`15-search-quality-evaluation-and-retrieval-research.md`](15-search-quality-evaluation-and-retrieval-research.md), [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md), [`17-official-public-api-and-sdks.md`](17-official-public-api-and-sdks.md), [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md), [`20-configuration-control-plane.md`](20-configuration-control-plane.md), [`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md), [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md), [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md), [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md), [`25-code-intelligence-indexing-crate.md`](25-code-intelligence-indexing-crate.md), [`26-observability-accounting-and-usage.md`](26-observability-accounting-and-usage.md), [`27-cross-host-agent-plugin-bundles.md`](27-cross-host-agent-plugin-bundles.md), and [`28-remote-multi-machine-shared-brain.md`](28-remote-multi-machine-shared-brain.md).

## 1. Program objective

The rewrite succeeds only if TraceDecay stops behaving like a collection of adjacent products that happen to share a binary. V2 must reconcile capture, LCM, sessions, code intelligence, Git, memory, analytics, automations, tasks/plans/executors, hints, tools, API, and dashboard into one system with:

- one authoritative owner for every concept and side effect;
- one immutable evidence path from every source into canonical observations;
- one identity and scope language across profile, repository, project, checkout, worktree, ref, provider, session, agent, and historical snapshot;
- one query/search/graph algebra and one result/coverage contract;
- one versioned policy and replay substrate;
- one generated capability catalog;
- one transport-neutral application command/query layer;
- thin CLI, MCP, HTTP, SDK, hook, and UI adapters;
- bounded, versioned extension points rather than copies or special cases;
- explicit scale, concurrency, privacy, reliability, and complexity budgets;
- a mandatory retirement path for every V1 implementation and temporary adapter.

The target is not merely fewer files. It is less semantic entropy: fewer competing meanings, fewer hidden defaults, fewer duplicated state machines, fewer untyped strings, fewer paths around policy, and fewer ways for two clients to receive different answers to the same question.

## 2. Evidence that convergence is required

### 2.1 Live planning probe

A TraceDecay context lookup against the planning worktree failed with an identity-cutover conflict: the same checkout resolved to both a selected store and a legacy store, each healthy and each containing materially different graph, fact, session, message, LCM, branch, automation, and payload counts. Retrying with an explicit project ID still re-entered the path resolver and returned the same conflict.

This is the architecture problem in miniature:

1. More than one store can appear authoritative for one logical identity.
2. Resolution and tool execution do not share one decisive scope result.
3. An explicit identifier is not always sufficient to bypass implicit path/CWD resolution.
4. Health is reported per shard, but the user needs a reconciliation decision for the logical system.
5. The safe behavior—preserve both and demand consolidation—is correct, but the recovery is not yet a first-class application workflow with a typed plan, preview, receipt, and postcondition.

The final 0.0.47 refusal quantified the fragmentation. Selected `proj_ceaa713e40fef2b2` was healthy with 38,510 nodes, 987 files, 17 facts, 2,003 sessions, 432,790 messages, 419,887 LCM rows, 14 branches, `automation_files=0`, five payload files, and three response files. Legacy `proj_b4a8bbe4953823c4` was also healthy with 36,596 nodes, 989 files, 129 facts, 4,129 sessions, 603,866 messages, 592,594 LCM rows, 197 branches, 3,470 automation files, 1,839 payload files, and four response files. The missing automation lane in selected and large legacy-only lane are coverage, not evidence that either shard is globally current.

Merged PR #425 (`de3d05dc`, final head `d3bb28b5`) is accepted V1 behavior: offline plan/apply, canonical platform paths, frozen SQLite families, final path-plus-file/inode holder refusal, reservations, dual backups, deterministic confirmation, restartable ledger/staging, explicit table dispositions/collisions, remapped LCM edges, exhaustive verification, marker/registry cutover, and doctor recovery. V2 absorbs and generalizes those invariants behind operation-specific plan/start/recover use cases; it does not keep a second consolidation authority or generic preview/apply framework.

V2 must retain the safe refusal while making ambiguity inspectable and repairable through the same canonical identity, command, status, and receipt contracts used by CLI, MCP, API, SDKs, and dashboard.

The final simplification probe on 0.0.53 adds a second concrete baseline. The current workspace is already one Rust package, yet it contains 59 top-level library modules, 416 Rust source files, roughly 267,715 source lines, a 286-file strongly connected component in the indexed graph, and a 7,108/10,000 health score whose weakest dimensions are acyclicity (0.5067; 2,475 cyclic edges) and equality (0.383; Gini 0.617). `src/global_db.rs` is 4,904 lines; `src/mcp/server.rs` is 3,284; `src/mcp/tools/definitions.rs` is 3,874; `src/sessions/lcm/query.rs` is 3,534. A Rust-scoped redundancy scan found 6,255 candidates, including copied host installers, extractor traversal/result builders, database-error helpers, scalar parsers, and row decoders. Therefore package count alone is not convergence: V2 packages must be dependency firewalls, while actual footprint reduction comes from generated descriptors, shared mechanics, and deletion of the implementations they replace.

### 2.2 Fragmentation inventory

The Phase 0 inventory generator must produce this table from source, schemas, routes, catalogs, configs, and store manifests. The human rows below establish the minimum audit surface.

| Area | Existing fragmentation to inventory | Canonical V2 owner | Required retirement proof |
|---|---|---|---|
| Physical stores | Global/session databases, project stores, LCM stores, code graph stores, analytics tables, payload directories, automation artifacts, legacy identity shards, WAL/recovery generations, and external host stores observed as evidence | `tracedecay-store` physical layout plus catalog/activity/project/graph/blob ownership rules | Every TraceDecay-owned store carries a plan 12 §14.1 disposition (`retained` — including read-only archives — `skipped`, `quarantined`, `redacted`, or `deleted`) plus a plan 12 PR 3R route status; every external store carries source ownership and read-only evidence status, and no unowned TraceDecay store opens after cutover |
| Sessions and LCM | Provider transcript ingestion, global session/message projection, V1 LCM native rows, summary DAGs, compression payloads, search tables, workflow/subagent ingestion | Sanitized capture observations plus profile activity projections; LCM is context lineage, not a second session authority | V1 session and LCM readers removed after parity and rollback window; one entity/retrieval ID loads sanitized-native message, summary lineage, and projection; protected plaintext is quarantine-only |
| Tasks, plans, boards, and execution | Provider goals/plans/workflows, automation jobs, advisory work claims, Hermes board DBs/current selector, per-repo tickets, assignee strings, host processes, worktrees/branches, executor queues, task-like dashboard/plugin state | One profile activity-shard initiative/plan/work-item event graph plus typed dependencies, assignments, fenced leases/attempts, executor SPI/routes, context packets, evidence relations, and saved query projections from plan 24 | One scheduler/lease owner; boards copy no task rows; ambient board/CWD never routes; every stale epoch is rejected; external/provider task evidence stays linked read-only unless a bounded import is separately approved; only TraceDecay-owned legacy dispatch/current-file/direct-DB paths are deleted |
| Provider capture | Per-provider scanners, hook records, workflow ingestion, Git correlation, automation import, ad hoc backfill markers | `tracedecay-capture` adapter registry and one observation journal | Every adapter passes one conformance suite; direct canonical writes and provider-specific redaction/store logic deleted |
| Identity | Path hashes, project keys, registry rows, worktree discovery, remote aliases, store markers, provider-local session IDs | `tracedecay-domain` IDs and `tracedecay-store` allocation/alias ledger | No public API accepts ambiguous `project_key`; no crate derives canonical IDs independently |
| Scope | CWD defaults, project selectors, registry search, worktree/ref selection, profile/global modes, tool-specific flags | `ScopeSelectorV2` plus one application resolver | Explicit scope never silently falls back; all transports pass the same scope conformance corpus |
| Code intelligence | Extraction, code graph, AST search, text search, diagnostics mapping, context assembly, dependency import, PR-context branch resolution | Capture/projectors/query/application in their bounded roles | Root/V1 graph query paths and direct DB calls removed; graph generation and snapshot IDs required in results |
| Query | Session search, LCM search, memory search, code search, SQL-shaped dashboards, graph traversals, context tools, exports | Domain `TraceQueryV1` AST plus `tracedecay-query` parser, planner, operators, rank pipeline, cursor, explain | No transport or UI builds SQL/query semantics; parity and quality gates prove replacement |
| Search ranking | Exact/FTS/BM25-like paths, fuzzy matching, embeddings, graph expansion, copied-message behavior, per-tool filtering | Versioned retrieval pipeline in `tracedecay-query`, evaluated by plan 15 | All rankers registered/versioned; no unmeasured ranking fork remains |
| Evidence and relations | Provider facts, correlation records, Git links, memory provenance, agent trees, tool results, code impact, PR links | Immutable observations plus bitemporal `RelationAssertion` and deterministic projections | Correlation never becomes fact by transport formatting; legacy relation tables are imported or retired |
| Policy | Hint classification, routing, retrieval choices, curation, memory injection, diagnostics, scheduling, coordination, automation decisions | `tracedecay-policy` bundles and deterministic replay | Every live decision identifies policy bundle/evaluator/input digest; ad hoc condition stacks removed |
| Hooks | Host-specific scripts, event matchers, spool behavior, hint rendering, acknowledgement, latency/error behavior | Private root `v2::hooks` module over capture/application/policy ports | Hosts pass one conformance suite; hook cannot own query, indexing, migration, or long-running work; copied installers/config writers are deleted |
| Tools/capabilities and host bundles | CLI commands, MCP tool names/schemas, HTTP routes, dashboard actions, skills, hook hints, aliases, host package/component manifests, and copied provider overlays | `tracedecay-tool-catalog` source of truth, including its pure `host_bundles` compiler; root-private deploy adapter owns host I/O only | Catalog/bundle generation covers every public action and supported host projection; hand-maintained semantic/manifest copies fail CI and retire in PR 37K |
| Application behavior | Mutations and queries embedded in CLI, MCP, dashboard routes, daemon tasks, doctor/remediation, installers | `tracedecay-application` use cases | Transports contain binding/rendering only; behavior conformance proves identical outcomes |
| Transports | CLI output/flags, MCP JSON/Markdown, HTTP envelopes, SSE events, SDK helpers | Thin adapters generated from catalog/application/API contracts | Semantic drift suite passes; stale clients fail explicitly before store access |
| Dashboard | Per-project pages, bespoke SQL endpoints, duplicated filters, separate graph products, action-specific state | V2 workbench over generated client and shared investigation state | No frontend data adapter bypasses the official client; legacy shell/routes retired after parity |
| Configuration | CLI flags, env vars, project/profile config, provider metadata, dashboard settings, hook config, daemon defaults | Typed versioned configuration resolver in application/root composition | Every effective value reports source/precedence/restart effect; no provider record weakens global safety floor |
| Analytics | Hook counts, session usage, savings, policy metrics, store health, automation runs, errors, dashboard aggregates | Plan-26 observation-derived accounting/observability projections using plan-08 generated surface codes and registered metric semantics | Denominators, coverage, cap/horizon/methodology/version, replay exclusion, and freshness required; bespoke counter writers and local surface enums deleted |
| Status/health | Doctor, diagnostics, store health, index freshness, LCM status, dashboard badges, daemon/service checks | Typed `SystemStatusSnapshot` assembled by application services | Same status facts and remediation IDs render on every surface; no health inferred from incidental row existence |
| Errors | Domain errors, SQLite strings, anyhow chains, CLI exit text, MCP errors, HTTP codes, dashboard toasts | One layered error taxonomy and generated transport mappings | Every public error has stable code, retryability, safe context, remediation capability, and trace ID |
| IDs/handles | Path hashes, row IDs, provider IDs, response handles, session IDs, retrieval IDs, graph IDs, URL parameters | Domain newtypes and global retrieval-anchor resolver | Strings are not interchanged accidentally; response handles never become sole durable citations |
| Privacy/redaction | Optional LCM redactor, memory secret rejection, remote URL omission, provider redaction markers, output-specific scrubbing | Mandatory sanitizer and typed safe-content boundary from plan 18 | Every old detector becomes fixture/reference or a plugin behind the one boundary, then is deleted |

### 2.3 Inventory artifact contract

Phase 0 generates `target/tracedecay-v2-inventory/` artifacts, never hand-edited production manifests:

- `stores.json`: location class, owner, schema/version, identity candidates, size, health, privacy domain, writer/readers, migration state;
- `tables.json`: table/index/trigger/FTS owner, reader/writer call sites, canonical target;
- `public-surfaces.json`: CLI, MCP, HTTP, SSE, SDK, dashboard, skill, hook, installer, config, and file-format surfaces;
- `semantic-implementations.json`: ID derivation, scope resolution, redaction, search/ranking, hinting, status, error mapping, config resolution, retry, and rendering implementations;
- `reuse-dispositions.json`: every duplicate/near-duplicate cluster and infrastructure mechanism with `retain | extract | declarativize | generate | replace | delete`, owner, target, evidence, parity gate, and deletion PR;
- `dependency-graph.json`: crate/module dependency edges, cycles, forbidden imports, SQL/file-system/network use;
- `footprint-baseline.json`: packages/published artifacts, handwritten/generated production lines, files/public items, dependencies/features, duplicate clusters, tables/indexes/triggers, workers, binary/assets, idle RSS, startup, clean/hot build, and stored file/byte counts;
- `adapter-ledger.json`: every anti-corruption adapter with owner, creation PR, traffic, parity gate, rollback dependency, and deletion PR;
- `convergence-scorecard.json`: metrics in Section 13 with baseline and target;
- `inventory.md`: safe human summary with no store content or secret candidates.

Plan 12's PR 3R compatibility-inventory generator is the single inventory generator; the artifacts above are generated views of that same run, and this plan consumes them — it does not build a second generator. Per-entity/store dispositions use plan 12 §14.1's five-value vocabulary (`retained | skipped | quarantined | redacted | deleted`); route/migration status uses PR 3R's six-value status axis (`v1_only` … `retired`). This authority relation is stated identically in plan 12 §17 PR 3R.

The inventory records symbols and schema names, not private content. It uses supported readers and manifests; it does not crawl raw databases as an implementation shortcut.

## 3. Governing architecture rules

1. **One meaning, one owner.** A concept has one canonical type and one crate responsible for its invariants.
2. **One effect, one route.** A side effect enters through one application command; adapters cannot reimplement it.
3. **Evidence first.** Sources produce immutable observations before mutable projections or policy decisions.
4. **Projection, not duplication.** Read models may repeat derived fields for performance but never become competing authority; every row carries source/projection versions and watermarks.
5. **Explicit scope.** CWD is one input to resolution, never invisible authority after an explicit selector is supplied.
6. **Typed boundaries.** IDs, safe text, cursors, scopes, errors, status, commands, and query results cross crates as domain/application types, not unvalidated strings or JSON blobs.
7. **Thin transports.** CLI, MCP, HTTP, SSE, SDK, hook, and UI bind and render application behavior.
8. **Generated parity.** Repeated public schemas and capability metadata are generated from one contract IR.
9. **Extensions use SPIs.** New providers, detectors, projectors, operators, policies, and UI contributions register through bounded contracts with budgets and provenance.
10. **Local-first daemon authority.** One installed product may supply daemon and client entry points, but only the daemon service constructs store/query/application authority. Local CLI/MCP/hooks/dashboard/SDK use authenticated IPC/API; remote/federated nodes reuse the same semantics. No production client embeds or opens stores.
11. **No permanent bridge.** Every compatibility adapter has a deletion gate when created.
12. **Safe failure.** Ambiguity, partial coverage, stale generations, privacy uncertainty, budget exhaustion, and version mismatch are visible typed states, not fallback triggers.
13. **Reuse mechanics, not meanings.** Registry/digest, projector, operation-fencing, host-install, extraction traversal, graph/timeline slice, rendering, cursor/page, and problem-envelope machinery has one implementation; domain admission, semantics, and state machines remain with their declared owner.
14. **Default to a module.** A new Rust package requires two independent production consumers or a demonstrated optional-heavy/deployment/publication capability firewall. Root-only adapters stay private modules with the same forbidden-edge linting.
15. **Replacement must delete.** Every new abstraction names the existing implementations, dependencies, schemas, tests, and adapters it removes; moving or wrapping them is not convergence.
16. **External evidence remains source-owned.** Host transcripts, project stores, board databases, caches, and backups are read-only evidence or prior art until an explicit bounded import decision names owner, reason, evidence, and rollback. TraceDecay retirement removes only TraceDecay-owned adapters/routes/copies; it never deletes Hermes-owned data.

### 3.1 Reusable mechanism map

| Mechanism reused system-wide | Sole implementation owner | Declarative/domain inputs | Implementations retired |
|---|---|---|---|
| Canonical IDs, time, scope, evidence, privacy-safe values, watermarks, canonical encoding/digests | Narrow `tracedecay-domain` kernel | Owner registries and invariant-heavy validators | Per-subsystem newtypes, string codecs, hash builders, enum spellings |
| Registry identity/version/owner/schema/deprecation/cross-reference/loading/digest/drift | Domain registry substrate; each registry retains semantic ownership | Domain, capability, use-case, configuration, metric, problem/status, SPI manifests | Registry-local loaders, canonicalizers, digest/version/replacement/codegen plumbing |
| Observation adapter framing/offset/rewrite/sanitize/publish conformance | `tracedecay-capture` | Provider/source descriptors and parsers | Provider runner switches, direct writes, provider-specific redaction/storage |
| Durable framed-segment append/fsync/torn-tail/rotation mechanics | `tracedecay-capture::framed_log` mechanical kernel | Capture spool frame registry/policy; root lifecycle-bootstrap receipt registry with separate service-only root/key | A second lifecycle journal engine, provider-local spool codecs, ad hoc CRC/hash-chain/recovery loops |
| Projection lease/checkpoint/gap/dead-letter/rebuild/publication/lag runtime | `tracedecay-projectors` | `ProjectionSpecV1` plus domain reducers/builders | Accounting/code/session/knowledge/automation runner and retry forks |
| Tree-sitter parse/traversal/result construction | `tracedecay-code-index` | Grammar descriptors and language-family query packs/hooks | Repeated `visit_children`, `build_result`, `extract_source`, C/C++ and BASIC helper bodies |
| Query cursor/budget/page, lexical/hybrid/rank/time/graph operators | `tracedecay-query` | Registered query profiles/operators | LCM/memory/context/dashboard/tool-specific ranking, limit, pagination, traversal forks |
| Pure evaluator/replay runtime | `tracedecay-policy` | Registered hint/retrieval/curation/routing/scheduler evaluators | Condition stacks and evaluator-specific replay harnesses |
| Fenced durable operation lifecycle | `tracedecay-application` | Domain-specific admission, state, steps, compensation/effect policy | Migration, export, repair, index, automation, task, daemon job/lock/progress engines |
| Hermetic experiment/run/trace/comparison/minimization harness | `tracedecay-application` over the operation kernel and policy evaluator registry | `ExperimentSpecV1`, catalog evaluator schemas, immutable manifests, explicit model/egress grants | Per-lab run tables/routes/schedulers, A/B vocabularies, replay sandboxes, progress/cancel paths, fixture minimizers |
| Wakeup/coalescing/backoff/fairness/checkpoint/fenced-admission scheduler | `tracedecay-application` `SchedulerKernelV1` | Task, automation, scout, maintenance, and index-trigger scheduling policies | Poll loops, per-feature timers/queues/backoff, scheduler-local checkpoint and fairness engines |
| Capability/use-case/binding/schema generation | `tracedecay-tool-catalog` plus plan-17 contract IR | One reviewed capability manifest | MCP definitions, format/allow lists, CLI/API/SDK/host permission side registries |
| Hook wire decode/normalize/response framing | Private root `v2::hooks` | `HostIntegrationManifestV1` hook facet plus irreducible host wire mappings | Hook-point switches, host response helpers, hook tool-name/permission side lists |
| Host install/update/uninstall/config mutation | One root host-integration engine outside the hook hot path | `HostIntegrationManifestV1` installation facet: paths, formats, ownership, backup and health probes | Nine installer copies, exact Cline/Roo duplicates, provider config mutation forks |
| Protected remote Brain transport wire | Private root `v2::remote_brain_transport` | Application/API/client contracts plus plan-28 connection/stream envelopes | TLS/mTLS listener/client, connection lifecycle, SSE and semantic snapshot/tail framing only; no enrollment, grant, placement, consistency, fencing, sync-policy, query, or store semantics |
| Sealed page/problem/graph/timeline views and human document rendering | Application view types; private root `v2::presentation` renderer | Catalog presentation descriptors and domain lens schemas | Handler envelopes/renderers, dashboard/MCP/CLI graph transforms, raw-value Markdown |
| Visual-semantic ontology, linked per-slot composition state, graph/timeline/metric envelope, interaction/query delta, accessibility/export capabilities | Application/catalog view contracts plus thin dashboard `WorkspaceSlotFrame` and renderer capability registry | Domain lens/entity/edge/lane/metric descriptors and five registered compositions | Switch-heavy universal renderer, feature-local chart wrappers, legends, selection/filter stores, workers/exporters, graph response types, lab visualization shells |

No row authorizes a generic `common` crate. If two uses only look similar but have different invariants, their owners keep separate typed implementations and the reuse-disposition ledger records `retain` with evidence.

## 4. Target canonical planes

### 4.1 Ingestion and evidence plane

`tracedecay-capture` owns source discovery, framing, parser/adapter execution, sanitization invocation, source offsets/generations, and construction of `ObservationEnvelopeV1`. `tracedecay-store` owns atomic journal publication, blob/quarantine persistence, outbox records, and acknowledgements. `tracedecay-projectors` alone converts observations into read models.

Required convergence:

- Provider transcripts, hook events, Git snapshots, code extraction, diagnostics, workflows, LCM V1, automation, memory imports, and legacy stores all enter through an adapter registry.
- One deterministic observation-ID function lives in domain; no adapter invents another UUID namespace or canonical encoder.
- Duplicate, late, rewritten, malformed, unavailable, quarantined, and unsupported records remain explicit evidence states.
- Canonical activity is written once. Project-attributed projections contain locators and derived indexes, not duplicate message bodies.
- Every projector is idempotent, deterministic for a pinned observation range/config/version, rebuildable, and watermarked.
- Direct writes from hooks/providers/transports into session, LCM, graph, analytics, memory, or dashboard stores are prohibited by architecture tests.

### 4.2 Identity and scope plane

`tracedecay-domain` defines `ProfileId`, `RepositoryId`, `ProjectId`, `CheckoutId`, `WorktreeId`, `RefId`, `CodeSnapshotId`, `GraphGenerationId`, `SourceInstanceId`, `ProviderId`, `ActorId`, `AgentId`, `SessionId`, `ThreadId`, `TurnId`, `MessageId`, `ObservationId`, `EntityId`, `RelationId`, `PolicyBundleId`, `CapabilityId`, and `RetrievalAnchorId`. `tracedecay-store` persists allocation and alias history. `tracedecay-application` resolves user selectors.

Required convergence:

- One `ScopeSelectorV2` serves CLI, MCP, API, SDKs, dashboard, hooks, jobs, and saved views.
- Resolution accepts stable IDs plus names, paths, remotes, branches, worktrees, PRs, collections, agents, and sessions as evidence-backed aliases.
- Explicit IDs bypass implicit CWD identity selection after access validation. Explicit paths resolve exactly or return candidates; they never collapse to the current project.
- A resolution result pins canonical IDs, candidate evidence, snapshot/ref generation, store routes, access decision, freshness, and ambiguity state.
- Resolution happens once per request. Downstream query, policy, and transport code receives `ScopeResolutionV2`, never repeats path/registry discovery.
- Identity reconciliation is an application workflow: preview candidates, compare coverage, choose merge/link/keep-separate, run resumably, emit receipt, verify postconditions, and preserve rollback sources.

### 4.3 Storage and projection plane

Physical federation remains explicit:

- profile `catalog.db`: identity allocations, content-free keyed alias-routing projections, store registry, schema/catalog versions, entity/anchor routes, migration receipts; canonical alias values/history do not live here;
- profile `activity.db`: observations and canonical provider/agent/session/Turn/message/workflow/goal activity plus cross-project/profile knowledge and automation;
- repository/privacy-domain `project.db`: code/Git/delivery evidence and explicitly project-scoped projections;
- immutable graph generations: packed snapshot-scoped graph data;
- privacy-domain content-addressed blobs: sanitized eligible payloads plus separate protected quarantine when explicitly enabled.

There is one logical system, not one giant SQLite file. `tracedecay-query` federates shards through declared capabilities and watermarks; transactions remain local. Cross-shard commands use journal/outbox/saga semantics and report incomplete compensation instead of pretending to be atomic.

Every projection declares:

- stable projection ID and owner crate/module;
- input observation/event kinds;
- schema and algorithm version;
- output store/shard class;
- watermark and lag contract;
- rebuild/checkpoint/rollback strategy;
- privacy eligibility and retention behavior;
- query operators and capability IDs it serves;
- parity corpus and performance budget.

### 4.4 Query, search, and graph plane

`tracedecay-domain` owns the one canonical `TraceQueryV1` AST/value/schema contract. `tracedecay-query` owns parsing, validation, canonicalization, planning, cost/budget enforcement, shard pruning, distributed cursors, graph/time/as-of operators, lexical/hybrid retrieval, ranking, diversity, explanation, and coverage reporting.

Required convergence:

- Session, LCM, memory, code, diagnostics, Git, agent, automation, facts, skills, and analytics queries compile from the same typed AST or call a specialized facade that compiles to it.
- Text search uses one versioned pipeline with exact/phrase/lexical foundations and optional measured fuzzy/entity/graph/dense/learned-sparse/rerank channels.
- Code graph, Git graph, thread graph, agent graph, Turn graph, timeline graph, knowledge graph, and automation graph share entity/relation/time/provenance primitives while retaining domain-specific operators. `RelationAssertionV1` is the one canonical entity-edge authority; domain-specific edge tables are rebuildable typed indexes with mandatory source relation IDs, never parallel truth.
- A query response always returns rows/nodes/edges plus pinned scope, coverage, freshness, watermarks, truncation, cost, planner/ranker versions, explanation, and stable retrieval anchors.
- No UI endpoint, MCP tool, or CLI command embeds SQL, FTS syntax, graph traversal, ranking, pagination, or store routing.
- Query caches key on normalized AST, resolved scope, access decision, snapshot/watermarks, representation/ranker versions, and privacy policy digest.

### 4.5 Policy and replay plane

`tracedecay-policy` owns deterministic evaluators for hints, retrieval routing, correlation, diagnostics, curation, memory, scheduler, automation, and nearby-agent coordination. It consumes immutable inputs and returns decisions/proposed effects. `tracedecay-application` revalidates effects; its curation worker autonomously applies every eligible owned fact/memory/managed-skill/profile-curation effect, monitors outcomes, and automatically revises/recovers. No item approval/apply command exists.

Required convergence:

- A `PolicyBundle` pins evaluator versions, configuration, catalog, index/snapshot watermarks, memory/skill versions, seed, time source, and budgets.
- Evaluation cannot write stores, call transports, read ambient CWD, or silently fetch live state.
- Exact replay uses matching artifacts; recorded replay returns stored decisions; best-effort replay declares every substitution.
- Labs and offline evaluation use the same evaluator path as live operation inside the one hermetic experiment harness: immutable mounts, frozen clock/RNG, disposable overlay, deny-by-default capabilities, explicit model/egress grants, shared operation lifecycle, and a `ReplaySideEffectReceiptV1` proving zero production effects. No evaluator owns another run table/route/scheduler/comparison/minimizer.
- Hint/retrieval/coordination analytics distinguish eligible, emitted, suppressed, acted-on, useful, false-positive, repeated, ignored, and outcome-unknown states.
- Policy code cannot define capability names, scope rules, redaction rules, query ranking, or output rendering independently.

### 4.6 Capability catalog

`tracedecay-tool-catalog` is the single registry for user/agent-visible capabilities. Each `CapabilityDefinition` owns:

- stable ID, semantic version, status, owner, aliases, and replacement;
- use-case command/query type and result/error schemas;
- allowed scopes, access requirements, privacy class, side-effect class, idempotency, retry policy, and budgets;
- CLI/MCP/HTTP/SDK/dashboard/hook/skill bindings;
- availability requirements and degraded/partial states;
- human/agent discovery phrases and examples;
- telemetry event IDs and conformance fixtures.

Catalog generation produces CLI metadata, MCP schemas, OpenAPI/JSON Schema references, SDK method manifests, dashboard action metadata, skill/hint discovery, docs, and drift tests. It does not generate business behavior; every binding resolves to one application use case.

The plan-27 host-bundle compiler is a pure module family inside `tracedecay-tool-catalog::host_bundles`, not a new crate, package, registry, or semantic manifest. It lowers the same `HostIntegrationManifestV1` and catalog snapshot into deterministic per-host/package trees, signed-manifest inputs, component/source maps, capability/difference/conformance reports, and stock-host fixtures. It performs no host discovery, filesystem/config mutation, marketplace publication, credential access, process launch, or install state transition. The root-private `v2::host_deploy` adapter alone probes hosts and applies the compiler's resolved artifacts through application operations, protected backups, atomic replacement, verification, compensation, and receipt-owned removal. This split preserves the eleven-package ceiling and makes package generation reusable without moving privileged host mechanics into the catalog crate.

### 4.7 Application command/query layer

`tracedecay-application` owns orchestration and is the only layer allowed to combine repositories, query services, policy evaluators, permissions, locks/leases, idempotency records, jobs, and audit effects into a public use case.

Each use case has:

- `Command` or `Query` input with `RequestContext`, explicit `ScopeSelectorV2`, access subject, idempotency key where applicable, deadline/budget, and expected version;
- one handler with injected ports;
- typed result, warnings, coverage, status deltas, audit receipt, and stable anchors;
- typed error variants mapped by generated transport tables;
- transaction/saga boundaries and retry semantics;
- conformance cases reusable by every transport;
- a declared capability ID.

Root composition wires implementations. Root may own bootstrap/process/service lifecycle and V1 anti-corruption adapters; it must not become a second application layer.

### 4.8 Thin transports, SDKs, and UI

Transport responsibilities are limited to authentication/session establishment, protocol handshake, input binding, deadline/cancellation propagation, streaming/framing, safe rendering, and transport-specific error/status mapping.

- CLI adds terminal formatting, exit codes, stdin/files, and shell completion.
- MCP adds JSON-RPC lifecycle, tool/resource binding, Markdown/JSON rendering, and protocol/catalog handshake.
- HTTP/SSE adds auth, request/response framing, cache headers, conditional requests, streaming, and OpenAPI.
- Rust/TypeScript/Python SDKs add idiomatic types, pagination/stream helpers, retry policy, cancellation, and debug-safe rendering.
- Hooks bind host events to the bounded spool/evaluation route and host response envelope.
- Dashboard uses only the generated TypeScript client plus UI-local view state; it never calls a hidden SQL or legacy endpoint.

Semantic conformance executes the same fixture through direct application invocation, CLI, MCP, HTTP, and SDK clients and compares normalized results, errors, warnings, coverage, anchors, and effects.

### 4.9 Security/redaction as the model convergence case

Redaction demonstrates why shared utilities alone are insufficient. Current behavior includes an optional LCM sanitizer, memory-specific secret rejection, Git-remote output omission, provider-native redaction markers, and tool-event content decisions. These paths answer different questions and permit gaps between input, storage, indexing, prompting, output, fixtures, and exports.

Plan 18 replaces them with:

1. One mandatory, versioned, parse-before-scan sanitizer before any TraceDecay persistence or agent exposure.
2. Domain taint types (`Unclassified`, `Classified`, `Sanitized`, and sink-specific eligible text) that make bypasses difficult to compile.
3. One detector registry and privacy-policy precedence model.
4. Sanitization receipts, coverage, quarantine, rescan, descendant invalidation, and secure-retirement workflows.
5. Sink-specific eligibility derived from the same sanitized result rather than independent regex calls.
6. Existing redactors/detectors retained only as fixtures/reference adapters until the canonical engine proves parity and stronger protection.

The same convergence pattern applies to identity, search, policy, config, status, and errors: preserve useful cases, establish one typed owner, adapt temporarily, prove parity, cut over, and delete the duplicate.

## 5. Canonical ownership matrix

| Concern | Defines contract | Executes behavior | Persists state | Exposes behavior |
|---|---|---|---|---|
| IDs, scope/time/evidence types | Domain | Application/capture/projectors/query as constrained | Store | All via generated schemas |
| Sanitized content eligibility | Domain/privacy contracts | Capture sanitizer | Store/quarantine | Application/API safe renderers |
| Source/provider adapters | Capture SPI | Capture | Store journal through injected port | Status/catalog only |
| Canonical observations | Domain | Capture | Store | Query/application |
| Identity allocation and canonical alias evidence | Domain | Application/store repository | Allocation ledger plus activity/project owner shards | Query/application |
| Keyed alias routing projection | Domain/store route contract | Projector/store | Content-free catalog routes only | Scope resolver |
| Projections | Projector registry | Projectors | Activity/project/graph stores | Query |
| Code extraction and immutable graph-generation builds | Domain code/evidence contracts plus plan-25 extractor registry | `tracedecay-code-index`; root only adapts its producer into the projector-owned build port | Durable source-fenced request; store generation writer outside SQLite; short manifest/pointer/checkpoint CAS transaction | Projector/query/application status; never a direct transport |
| Query AST/value/schema | Domain | Query parses/validates/canonicalizes | None | Query/application/generated bindings |
| Query planning/ranking/execution | Query | Query | Query cache/eval artifacts through ports | Application |
| Retrieval-evaluation truth | Domain retrieval-evaluation contracts plus plan-15 corpus/profile registries | Query evaluator under the generic hermetic experiment runner and application authorization | Store-owned activity-shard corpus, qrel, pool, judgment, adjudication, metric/report, fixture, and profile families plus shared experiment/run rows | Application, Search Quality Lab, Observatory, and generated bindings |
| Research manifests and durable anchors | Domain research/anchor contracts | Application manifest use cases plus query anchor resolver | Store owner-shard research manifests, entries, tombstones, and route metadata | Query/application and authorized CLI/MCP/API/SDK/UI views |
| Session/LCM temporal lineage and answers | Domain message/summary/`TraceQueryV1` temporal contracts | Capture/projectors/query/application in their bounded roles | Activity-shard native message and summary-lineage projections plus privacy-domain blobs | Query/application, Timeline, LCM Lab, and generated bindings |
| Policy bundles/evaluators | Policy | Policy | Policy artifacts/results through ports | Application/labs |
| Capability metadata | Tool catalog | Catalog generation/runtime lookup | Generated/catalog snapshots | All transports/UI/docs |
| Cross-host bundle projection and deployment | Tool-catalog `host_bundles` module defines pure lowering from canonical manifest/catalog; plan 27 fixes host overlay/conformance contracts | Pure compiler in tool catalog; root-private `v2::host_deploy` performs privileged local probe/stage/apply/atomic-activate/verify/repair/remove effects through application ports; plan 12/PR 36R alone owns release/marketplace publication | Signed release artifacts plus store-owned safe integration/operation projections; protected config backups remain outside general stores | Settings/doctor/generated bindings and native host packages; no second catalog or installer crate |
| Use-case semantics | Application | Application | Injected repositories/job/audit ledger | CLI/MCP/API/SDK/UI/hooks |
| Task/plan graph truth | Domain plan-24 graph/version/event contracts | Application commands and deterministic projectors | Activity-owner task event ledger and current projections | Query/application, Work/Resume, and generated bindings |
| Scheduling, offers, admission, leases, grants, and writable-resource reservations | Domain plan-24 lifecycle contracts; policy proposes only | Application scheduler/admission transaction and executor adapters | Activity-owner offers, assignments, attempts, fenced leases, grant sets, reservations, and receipts | Executor SPI, status/doctor, task views, and generated bindings |
| Context scouting and suggestion delivery | Domain suggestion/envelope contracts plus policy delivery arbiter | Application scout worker; hooks claim/deliver only the accepted envelope | Activity-owner candidates, envelopes, claims, delivery/outcome receipts, and checkpoints | Hint Lab, Observatory, status, and generated bindings |
| Accounting and observability semantics | Domain accounting contracts plus plan-26 metric-descriptor registry | Projectors/accounting services and application SLO monitors | Owner-shard accounting events and versioned accounting/operations/all-scope rollups | Observatory, Costs, status/doctor, and generated bindings |
| Human-facing Markdown/terminal presentation | Plan-21 root `v2::presentation` document/render contracts over catalog descriptors and sealed application views | Root-private pure renderers | None | CLI/MCP/root adapters |
| HTTP/SSE protocol envelopes and public contract artifacts | API/generated contract IR | Thin API adapter and generators | None except safe request audit through application ports | HTTP/SSE and official SDK packages |
| MCP lifecycle, primitives, progress/cancellation/tasks, and framing | Official MCP SDK boundary plus generated tool-catalog bindings | Root MCP adapter only | No protocol state beyond the connection; safe application audit/operation records use their canonical stores | MCP clients through negotiated tools/resources/prompts/completion/notifications/tasks |
| Official client transport runtimes | Generated public contract IR | Each Rust/TypeScript/Python client package | Client-local ephemeral transport state only | External callers; never in-process store/application access |
| Effective configuration control plane | Domain config value/provenance contracts plus plan-20 registry | Application resolver/commands; root bootstrap only supplies sources | Profile/project config versions, history, impact, and audit repository | Status/settings/all transports |
| System status/remediation | Application typed models | Application | Observability projections/audit | All transports/UI |
| Error semantics | Domain/application error taxonomy | Owning layer | Safe error/audit projection | Generated mapping/rendering |
| UI information architecture | Frontend | Frontend view models/interactions | Saved-view command only | Browser |

No row may gain a second owner without an ADR that explains why it is a distinct bounded concept rather than a convenience copy.

## 6. Crate and module dependency DAG

### 6.1 Target workspace

```text
crates/
├── tracedecay-domain/          # pure canonical types, invariants, schemas, no I/O
├── tracedecay-store/           # repository implementations, migrations, journal, blobs
├── tracedecay-capture/         # source SPIs/adapters, normalization, privacy engine, spools
├── tracedecay-projectors/      # deterministic observation -> projection handlers
├── tracedecay-code-index/      # code extraction, incremental indexing, packed graph-generation builds (plan 25)
├── tracedecay-query/           # TraceQueryV1 parser/execution, federation, search, graph/time, explain
├── tracedecay-policy/          # pure versioned evaluators and replay
├── tracedecay-tool-catalog/    # capability IR, validation, generators, runtime snapshot
│   └── src/host_bundles/       # pure canonical-manifest -> host package compiler/conformance artifacts (plan 27)
├── tracedecay-application/     # commands, queries, workflows, ports, typed status/errors
└── tracedecay-client/          # official Rust transport client over generated public contracts only
src/                            # root binary/composition, CLI/MCP/daemon/install/update, V1 adapters
└── v2/
    ├── hooks/                  # private bounded host event/delivery adapter (plan 07)
    ├── host_deploy/            # private local probe/stage/apply/activate/verify/repair/remove adapter over compiled bundles; no release publication
    ├── presentation/           # private sealed-view -> document/terminal/Markdown renderer (plan 21)
    ├── api/                    # private Axum HTTP/SSE/OpenAPI host adapter (plan 10)
    └── remote_brain_transport/ # private HTTPS/mTLS and semantic sync wire adapter (plan 28); no authority/store semantics
dashboard/                      # workbench using generated TypeScript client
packages/tracedecay-client/     # official TypeScript client independent of dashboard state
python/tracedecay-client/       # official typed sync/async Python client
```

The target contains at most 11 Rust packages including root and `tracedecay-client`. Plans 07, 10, 21, 27, and 28 retain separate design documents because hook, API, presentation, cross-host bundle, and protected remote-transport contracts need independent ownership/tests, but host-bundle compilation remains a `tracedecay-tool-catalog` module and deployment/remote transport remain root-private modules; none becomes a separately published Rust package. Do not create a generic `core`, `common`, `utils`, `services`, `plugin`, `host-bundles`, or `remote-store` crate. Shared code moves to the package/module that owns its invariant. A new package requires:

- at least two independent production consumers **or** a demonstrated optional-heavy, deployment, publication, or public-client dependency firewall;
- a coherent domain or deployment boundary;
- a dependency direction that reduces, not hides, cycles;
- public contract and non-goals;
- independent tests/benchmarks only when it has independent behavior;
- an ADR and deletion/migration plan for code it replaces.

### 6.2 Compile-time allowed edges

```mermaid
flowchart TD
    D["tracedecay-domain"]
    S["tracedecay-store"] --> D
    C["tracedecay-capture"] --> D
    J["tracedecay-projectors"] --> D
    CI["tracedecay-code-index"] --> D
    Q["tracedecay-query"] --> D
    P["tracedecay-policy"] --> D
    T["tracedecay-tool-catalog"] --> D
    A["tracedecay-application"] --> D
    A --> S
    A --> C
    A --> J
    A --> Q
    A --> P
    A --> T
    CONTRACT["generated public contracts + ApiProblem"]
    NODECONTRACT["generated internal node protocol contracts"]
    CLIENT["tracedecay-client"] --> CONTRACT
    R["root composition and adapters"] --> A
    R --> S
    R --> C
    R --> J
    R --> CI
    R --> Q
    R --> P
    R --> T
    H["root::v2::hooks"] --> A
    H --> D
    H --> C
    H --> T
    HD["root::v2::host_deploy"] --> A
    HD --> D
    HD --> T
    PR["root::v2::presentation"] --> A
    PR --> D
    PR --> T
    API["root::v2::api"] --> A
    API --> D
    API --> T
    API --> CONTRACT
    RB["root::v2::remote_brain_transport"] --> A
    RB --> D
    RB --> NODECONTRACT
    API --> NODECONTRACT
    R --> H
    R --> HD
    R --> PR
    R --> API
    R --> RB
    TS["generated TypeScript client"] --> CONTRACT
    PY["generated Python client"] --> CONTRACT
    UI["dashboard"] --> TS
```

These arrows are compile-time import/generation edges, not network calls. The five `root::v2` nodes are module-lint boundaries inside the root package and cannot use root-private internals outside their declared imports. `host_deploy` consumes resolved artifacts from `tracedecay-tool-catalog::host_bundles`; it cannot compile or reinterpret them. `generated public contracts + ApiProblem` is the plan-17 contract-IR output materialized into each public client package; `generated internal node protocol contracts` is plan 10's private node-only IR and never enters public clients or agent tools. Neither is a server facade or a new business crate. The Rust `tracedecay-client` may import only public generated request/response/event/problem definitions and its small client-owned transport/pagination/stream runtime. It has no Cargo dependency on `tracedecay-domain`, `tracedecay-store`, `tracedecay-application`, or the root API implementation. The TypeScript and Python clients have the equivalent package boundary.

To preserve testability, repository and executor traits are owned by the consumer: capture owns `ObservationSink`, query owns read capabilities, projectors own projection sinks, and application owns orchestration ports. Concrete cross-crate adapters live in application/root composition, not in the lower-level crates.

### 6.3 Runtime transport flow

```mermaid
flowchart LR
    RC["Rust/TypeScript/Python client"] -->|"authenticated UDS/named pipe or HTTP/SSE"| API["daemon V2 API adapter"]
    UI["dashboard via TypeScript client"] -->|"authenticated HTTP/SSE"| API
    REMOTE["authorized remote public client"] -->|"HTTPS/mTLS"| API
    API -->|"generated request + caller context"| A["tracedecay-application"]
    A -->|"typed response/problem/event"| API
    API -->|"wire envelope"| RC
    API -->|"wire envelope"| UI
    API -->|"wire envelope"| REMOTE
    NODE["enrolled Brain node"] -->|"mTLS + internal node protocol"| RB["root remote Brain transport"]
    RB -->|"generated request + caller/node context"| A
    A -->|"typed receipt/snapshot/tail/problem"| RB
    RB -->|"bounded signed frame"| NODE
    ROOT["root CLI/MCP adapters"] -->|"authenticated daemon local protocol"| API
```

Runtime calls do not create compile dependencies in the opposite direction: application and API never import an SDK, and production clients never reach store/domain/application APIs in-process. The optional Rust in-process conformance transport implements the generated public client transport trait only in hermetic tests and cannot construct production `StoreFactory`. The enrolled-node path is separately generated and private; remote public clients never gain observation-upload, snapshot-page, tail, or acknowledgement bindings.

### 6.4 Publication consequences

Plan 12 owns release execution, but its publication manifest is generated from `architecture-boundaries.toml` and must be a topological projection of this DAG: `tracedecay-domain`; then the domain-only implementation crates (`tracedecay-store`, `tracedecay-capture`, `tracedecay-projectors`, `tracedecay-code-index`, `tracedecay-query`, `tracedecay-policy`, and `tracedecay-tool-catalog`); then `tracedecay-application`; then the official `tracedecay-client` from the same frozen generated-contract digest and the root package containing private hook/host-deploy/presentation/API/remote-Brain-transport modules. After the root artifact is fixed, `tracedecay-tool-catalog::host_bundles` deterministically emits the native host package set and conformance manifests for plan 12's component-atomic marketplace publication; these are release artifacts, not Rust packages. Peers in a wave may publish concurrently only when `cargo metadata` and generated-contract edges prove they are independent. Every artifact must become registry-readable with the expected checksum before a dependent wave starts. No crates.io package is created for a root-only adapter.

### 6.5 Forbidden edges and capabilities

- Domain imports no TraceDecay crate and performs no filesystem, database, network, process, clock, random, or ambient-environment I/O.
- Store contains no provider parser, ranking, policy, transport, dashboard, or remediation decisions.
- Capture contains no SQL/store implementation, projection, query, ranking, policy, transport, or dashboard code.
- Projectors contain no transport, UI, provider discovery, live network, policy decision, or ad hoc ID derivation.
- Query contains no writes to canonical stores, transport rendering, provider discovery, policy decisions, or ambient CWD resolution.
- Policy contains no store/network/filesystem/clock/random capability except injected deterministic inputs and bounded pure extension runtimes.
- The root hook module contains no broad graph scan, migration, indexing, automation, remote request, or direct store/query implementation.
- The user-facing thin integration binary `tracedecay`, MCP/dashboard client compositions, and official client modules cannot link store repositories, concrete V2 store constructors, `StoreFactory`, store layout/path helpers, or application implementations. Ordinary CLI/MCP/dashboard modes depend only on generated daemon-client contracts plus manifest-only service lifecycle bootstrap; explicitly profiled hook/spool/read-only-source-broker/user-effect-broker entries are narrow root adapters over capture/application contracts and cannot dispatch business/query behavior locally. The private `tracedecayd` binary and its service-manager-launched maintenance mode are the only compositions that construct TraceDecay store adapters. Provider-source SQLite readers are permitted only in the read-only user source broker. User-owned mutations use the one application `UserEffectPortV1` plus root race-safe effect adapter, not capture or ambient daemon filesystem access. Source/link lints prove both brokers cannot import TraceDecay store/layout/canonical-writer capabilities. This capability-specific rule replaces the impossible package-wide ban on every SQLite driver.
- Tool catalog contains metadata/validation/generation, never use-case execution.
- `tracedecay-tool-catalog::host_bundles` is pure: no host/cache/config discovery, filesystem mutation, credential access, network/marketplace call, process launch, install state, or private backup body.
- Root `v2::host_deploy` contains no capability/workflow/skill/hook/MCP semantics or manifest compiler; it applies signed resolved artifacts through application-owned operations and receipt-bounded I/O only.
- The root API module contains no business mutation, SQL, ranking, policy, provider parsing, or V1 fallback.
- Root `v2::remote_brain_transport` contains no enrollment, grant, placement, consistency, fencing, sync-policy, query, persistence, or public-client semantics; it performs authenticated connection lifecycle and bounded generated node-protocol framing before invoking application ports.
- Client packages contain no domain/store/application/server imports, SQL, scope resolution, routing, retry invention, scheduler logic, or in-process business calls; they serialize generated contracts and invoke the service at runtime.
- Root contains no new business rules; new behavior lands in its owning crate/application first.
- Dashboard contains no private endpoint client, SQL-shaped request, capability-name literal registry, or independent error/status semantics.

CI validates these constraints through `cargo metadata`, import/source scans, feature matrices, compile-fail tests, and a checked dependency policy file.

One checked `architecture-boundaries.toml` is the machine authority for packages/modules, owners, public facades, allowed/forbidden edges, capabilities, stores, release waves, budgets, replaced V1 clusters, and deletion PRs. It generates the dependency-policy file, release DAG, ownership documentation fragments, and scorecard skeleton. Hand-maintained topology copies are explanatory only and CI fails if they drift from the generated fragments.

## 7. Extension and plugin SPIs

### 7.1 Principle

Extensibility means adding a bounded implementation without copying a pipeline or editing every transport. It does not mean arbitrary code can mutate stores or introspect private content.

Every SPI has:

- stable namespaced ID and version range;
- manifest-declared inputs, outputs, source/effect/privacy classes, capabilities, resource budgets, and determinism;
- typed host calls and no access beyond declared capabilities;
- schema validation and conformance fixtures;
- executable/content digest and provenance;
- timeout, memory, output-size, cancellation, and failure-isolation rules;
- availability/status reporting;
- safe upgrade, disable, rollback, and state-migration behavior;
- compatibility policy that rejects unsupported major versions explicitly.

### 7.2 Supported SPIs

| SPI | Owner | Extension can do | Extension cannot do |
|---|---|---|---|
| Source adapter/parser | Capture | Discover declared source, frame records, parse into observation drafts | Allocate canonical IDs independently, write stores, weaken privacy, make policy decisions |
| Code extractor/grammar | Code index crate (plan 25), fed by capture-sanitized content | Produce typed syntax/symbol/edge observations for a snapshot | Query live project stores or publish graph generations directly |
| Secret detector | Capture privacy engine | Return protected spans/classes/confidence under sandbox and budgets | Emit candidate content, use network/filesystem, bypass mandatory built-ins |
| Projector | Projectors | Consume declared observation kinds and emit typed projection mutations | Read ambient state, call transports, mutate unrelated projections |
| Query operator | Query | Add a typed bounded operator with cost/coverage/explain implementation | Bypass scope/access/budget, return unanchored evidence, mutate state |
| Retrieval representation/ranker | Query | Build/version representation and score bounded candidates | Receive unauthorized content, silently become default, skip labeled evaluation |
| Policy evaluator | Policy | Evaluate pinned typed inputs and return decisions/proposed effects | Perform I/O/effects, invent capability IDs, hide substitutions |
| Output renderer | Transport-owned | Render typed safe view models | Fetch data, apply business rules, reveal protected fields |
| Dashboard contribution | Frontend registry | Register route/panel/lens for declared capability/view model | Call private endpoints, inject global CSS/state, bypass access/coverage semantics |
| Automation/skill provider | Application/policy/catalog | Register candidate/validation/autonomous-execution/monitoring/recovery capability with audit lifecycle | Execute outside configured authority, modify own evidence, access secrets, or create a per-item human gate |

### 7.3 Runtime tiers

1. **Built-in Rust:** first-party, compiled, full conformance, least runtime overhead.
2. **WASM component:** preferred untrusted/third-party pure transform/evaluator; capability-free by default with bounded host calls.
3. **Isolated subprocess:** only for extractors/tools needing native runtimes; authenticated framed protocol, sandbox profile, restricted environment/filesystem/network, hard budgets, and the shared daemon subprocess supervisor below.
4. **Remote extension:** deferred; requires explicit user configuration, authenticated protocol, privacy-domain egress policy, offline/degraded semantics, and threat-model ADR.

No unstable Rust dynamic-library ABI is a public plugin contract. WIT/JSON Schema/protobuf-like wire contracts are generated from the same versioned SPI IR where applicable. The first release may keep SPIs internal until two implementations and conformance suites prove the boundary; internal status must not be documented as stable public API.

All child-producing components reuse one root-private subprocess-supervision kernel; provider adapters, schedulers, executors, experiments, extractors, and extension hosts cannot own independent child registries or retry loops. Admission atomically registers before spawn, binds lifecycle epoch/cancellation, rejects late/retry spawn after drain, and terminates/reaps under one aggregate deadline. Linux uses a daemon-owned cgroup/service scope and Windows a kill-on-close Job Object; a process group or descendant scan alone is never containment because `setsid`/double-fork/reparenting can escape. macOS epoch one permits native subprocess execution only when a stock-host probe proves the packaged sandbox denies fork/spawn and the registered child requires exactly one process (`MacSandboxNoFork`); child kinds needing descendants use built-in Rust/WASM/remote execution or are `Unavailable`. Without that enforceable probe the supervisor returns `ContainmentUnproven`, refuses the spawn before effects where possible, and can never publish clean shutdown from observational scans. The supervisor persists plan-01 `SubprocessShutdownReceiptV1` through plan 02 and exports plan-26 metrics without becoming a public crate. FM-157 covers each registered child kind; architecture lint rejects direct spawn outside the kernel/test harness.

### 7.4 Extension registry and dependency rule

The capability catalog references extensions by ID/digest and exposes availability. Owning crates host the registries. Do not add a general extension-runtime crate until at least two owners share identical sandbox/protocol lifecycle behavior; if that threshold is reached, extract a narrow `tracedecay-extension-host` crate that depends only on domain wire contracts and contains no domain-specific policy.

## 8. Naming, schema, version, configuration, status, and error governance

### 8.1 Ubiquitous language

Maintain `docs/architecture/glossary.md` and machine-readable domain registry. Reserved terms have one meaning:

- **observation:** immutable source record plus provenance;
- **event:** canonical domain occurrence projected from evidence;
- **entity:** stable logical thing with aliases/occurrences;
- **relation assertion:** time-bound, sourced claim connecting entities;
- **projection:** rebuildable derived read model;
- **session:** provider/user interaction container;
- **Turn:** one agent execution unit, distinct from a message;
- **agent:** actor/runtime instance, distinct from provider/model/session;
- **project:** logical scoped workspace, distinct from repository/checkout/worktree;
- **scope:** explicit query/effect boundary;
- **snapshot/generation:** immutable pinned code/graph/index state;
- **capability:** discoverable public use case, distinct from transport binding;
- **policy decision:** deterministic evaluation output, distinct from applied effect;
- **retrieval anchor:** durable locator to retained evidence, distinct from response handle.

The registry forbids overloaded aliases in public schemas without a migration annotation. Rust types, JSON fields, CLI flags, MCP properties, OpenAPI, SDKs, UI labels, telemetry, and docs derive from or validate against the same vocabulary.

### 8.2 Schema governance

- Every persisted/event/API/SPI schema has a namespaced ID, semantic version, owner, compatibility class, privacy classification, and migration function or explicit non-migratable status.
- Additive optional changes are minor only when defaults do not change meaning. New required fields, changed units/meaning, or removed variants are major.
- Persisted observations retain original bytes only when eligible; canonical decoded representation records parser/schema version.
- Projection schema changes use new generation/backfill and atomic publication, not in-place semantic mutation without a receipt.
- Unknown enum variants/fields survive capture where the source format permits, but public handlers fail or degrade explicitly according to the schema contract.
- Golden schema snapshots, upgrade/downgrade fixtures, and API/SPI compatibility checks run in CI.

### 8.3 Configuration governance

One typed resolver evaluates built-in defaults, profile, project/privacy-domain, provider/source, environment, CLI/request override, and policy floor according to field-specific precedence. Each `EffectiveConfigValue<T>` carries value, source, version, validation, sensitivity, changeability, and restart/rebuild impact.

- Safety floors cannot be weakened downstream.
- Unknown keys and obsolete names are errors with current replacement/remediation.
- Secret values are references to an external protected mechanism, never status/debug content.
- Query/policy/replay pin effective-config digests.
- Dashboard settings, CLI config, doctor, hooks, daemon, automation, and SDKs use the same read/update application commands.

### 8.4 Status governance

One `SystemStatusSnapshot` assembles component states without hiding disagreement:

```text
component_id, owner, state, reason_code, observed_at,
coverage, freshness, watermark, configured_version, effective_version,
desired_version, dependencies, blocked_by, remediation_capability_id,
safe_details, retrieval_anchors
```

States include `Healthy`, `Degraded`, `Partial`, `Stale`, `Reconciling`, `Blocked`, `Quarantined`, `Unavailable`, and `Unknown`. “Healthy” cannot be inferred merely because a table has rows or a database opens. Conflicting identity stores, missing shards, unscanned privacy data, unsupported adapters, skipped sources, and lagging projections remain first-class components.

### 8.5 Error governance

Errors are layered without leaking implementation strings:

- domain invariant errors;
- repository/storage errors;
- capture/projection/query/policy errors;
- application use-case errors;
- transport binding/rendering errors.

Every public `TraceErrorV2` includes stable `code`, category, safe message, retryability, retry-after when applicable, capability/use-case ID, trace/request ID, safe structured details, cause class, partial-result/side-effect state, remediation capability ID, and retrieval anchors. Sensitive candidates, SQL, filesystem internals, raw provider records, tokens, and unbounded chains never enter public details.

Generated mapping enforces CLI exit code, MCP error, HTTP status/problem detail, SDK exception, SSE terminal event, and dashboard presentation parity. Tests assert semantic identity across transports.

## 9. Generated contracts and drift prevention

### 9.1 Contract IR inputs

- domain schema registry;
- capability catalog;
- application use-case registry;
- API route/event registry;
- SPI registry;
- error/status/remediation registries;
- configuration schema and vocabulary registry.

### 9.2 Generated outputs

- JSON Schema and OpenAPI;
- CLI command/flag/completion/reference metadata;
- MCP tool/resource/prompt schemas and discovery metadata;
- Rust/TypeScript/Python public types and method manifests;
- dashboard client, query keys, event discriminants, error/status maps, action registry;
- hook/provider binding manifests;
- managed-skill capability references and hint-discovery facts;
- docs/reference/examples with synthetic safe values;
- telemetry event/field registry;
- conformance vectors and compatibility snapshots.

### 9.3 Drift gates

CI regenerates into a temporary directory and fails on diff. It also fails when:

- a public route/command/tool/action lacks a capability ID;
- two bindings claim the same public name without an explicit alias/replacement relation;
- a hand-written transport schema conflicts with generated IR;
- an error/status/config enum is missing a transport mapping;
- a dashboard action calls an unregistered route;
- SDK/API schema digests differ from the binary/catalog handshake;
- a removed capability lacks migration/replacement and cutoff metadata;
- generated fixtures/examples fail the privacy scan.

Generated code remains mechanical. Human-written ergonomic SDK helpers and UI view models may wrap it but cannot redefine semantics.

## 10. Concurrency, sharding, and scale

### 10.1 Workload model

Design and benchmark at minimum:

- 128 simultaneous hook/agent producer lanes on one profile;
- agents split across the same worktree and parallel worktrees;
- hundreds of registered repositories/projects and many historical checkouts;
- millions of messages/tool events and large LCM summary DAGs;
- multiple code graph generations per branch/ref/worktree;
- concurrent capture, projection, query, backup, rescan, automation, and dashboard streaming;
- disk-full, locked database, corrupt tail, process crash, stale daemon, upgrade drain, and unavailable shard conditions.

### 10.2 Writer and consistency topology

- One daemon authority owns every ordinary mutable writer, read pool, checkpoint, integrity probe, and consistent snapshot for its placed profile/project shards. Client processes cannot link/open TraceDecay stores; the private service-identity maintenance mode may open after explicit inherited exclusive-authority handoff or after the lifecycle coordinator proves daemon death and advances a new fenced exclusive maintenance epoch.
- Strong isolation is `DedicatedServiceIdentity` or `RemoteAuthorityOnly`; OS owner/ACL/key/endpoint probes prove clients cannot traverse/read database families while authorized IPC still works. `SameUserDegraded` is honest portability state, not a security claim.
- Hooks append to per-producer durable spool segments with monotonic producer sequence and bounded synchronous deadline.
- Drainers publish observations idempotently and acknowledge only after journal durability.
- Journal/outbox drives projectors, representations, analytics, policy outcomes, and notifications.
- Readers pin vector watermarks across catalog/activity/project/graph/representation generations.
- Distributed/federated responses report per-shard coverage and staleness; they never claim one atomic snapshot when one was not available.
- Backpressure propagates typed states and preserves priority/reserved capacity for safety/ack records.
- Leases have owner, fencing token, expiry, heartbeat, takeover, and diagnostic history.

### 10.3 Shard and representation policy

- Shard by ownership/privacy/failure domain, not by whichever module first needs a database.
- Catalog routes logical scope to stores; query planner prunes before opening shards.
- Graph and search generations are immutable and atomically published.
- Large payloads are content-addressed in their privacy domain; projections carry safe locators/digests.
- Rebalancing/moving a repository creates a resumable copy/verify/publish/retire receipt without changing logical IDs.
- Plan 28's optional remote shared Brain reuses the same domain/store/application/query/API/client contracts: one fenced authority per mutable shard, host-local SQLite, semantic observation/snapshot/tail transfer, and explicit consistency/coverage. Root-private `v2::remote_brain_transport` adapts authenticated HTTPS/mTLS connections and snapshot/tail/SSE wire framing only. It adds no remote-store crate, second API, second query planner, second auth model, or Tailscale-specific business path. Future engine adapters must replace an existing physical mechanism behind these contracts and pass parity/fault/footprint gates before selection.

### 10.4 Performance budgets

Each plane publishes a benchmark manifest and current/10x/100x corpus results. At minimum gate:

- hook append plus mandatory safety floor meets the hook plan p95 target and never bypasses privacy on timeout;
- point identity/scope resolution does not scan all registered shards;
- common scoped list/search queries prune to the minimal shard set;
- cross-project query latency reports planning, shard-open, candidate, rank, hydration, and rendering components;
- projector throughput remains above sustained ingest with bounded recovery lag;
- graph/timeline queries enforce node/edge/time/memory budgets and stream progressive results;
- dashboard renderers enforce level-of-detail and main-thread/frame budgets;
- no benchmark improvement may trade away correctness, coverage truth, privacy, or deterministic replay.

The exact numeric SLOs remain those in owning plans and the master performance section. This document adds the requirement that every SLO identify one canonical measured path; V1/adapter paths cannot be averaged together to conceal a regression.

## 11. Organization and complexity budgets

### 11.1 Source layout budgets

- Production Rust/TypeScript files target at most 400 lines; 800 lines is a hard default ceiling.
- A file above 800 lines requires a temporary architecture waiver naming split owner, reason, and deletion PR; generated files and data-only registries are exempt but must be clearly generated.
- Functions target at most 60 lines and a hard default of 100; parsers/state machines may exceed only with focused tests and a documented reason.
- Cyclomatic complexity target is <=15 per function; higher values require decomposition or an explicit tested state-machine/table representation.
- Public functions target <=6 parameters; use typed request/context structs instead of positional growth.
- Nesting deeper than four control levels is rejected unless generated or a parser with a documented grammar.
- A module directory with more than 12 peer implementation files requires subdomain grouping and a `mod.rs`/README ownership map.
- One source file owns one primary responsibility; `utils`, `helpers`, `common`, `misc`, and numbered continuation files are prohibited as destinations for new behavior.

### 11.2 Dependency and API budgets

- Zero dependency cycles among V2 crates.
- Domain has zero runtime dependencies on database, async runtime, web, CLI, provider SDK, or filesystem libraries.
- Public API growth is measured per PR; additions require owner, use case, compatibility class, tests, and docs.
- Each crate publishes an `ARCHITECTURE.md` with responsibility, non-goals, public ports, allowed/forbidden edges, state ownership, and extension points.
- Internal module visibility is default; public re-exports occur from deliberate crate facades.
- Feature flags represent deployment/optional heavy capabilities, not contradictory semantics.

### 11.3 Review gates

CI/reporting records file/function/complexity deltas, new public items, new dependencies/features, unsafe blocks, SQL locations, stringly typed IDs, duplicate detectors/resolvers/rankers, and adapter count. A budget violation blocks the slice unless the waiver is reviewed with the architecture owner and has a specific expiry.

### 11.4 Reuse, negative-code, and footprint budgets

- PR 1 freezes `footprint-baseline.json`; every implementation PR emits a comparable delta and classifies work as `parity_replacement`, `net_new_product`, `generated`, `migration_only`, or `test_fixture`.
- A parity-replacement lane cannot cut over until handwritten live V2 lines are lower than the V1 plus adapter lines it retires. Generated output is reported separately with generator handwritten size and generation time; generated bulk cannot manufacture a favorable ratio.
- Every new package, public item, dependency/feature, table/index/trigger, background worker, cache, and durable file family names the existing mechanism it replaces or a net-new requirement. Package count is capped at 11 including root/client; a new package requires a reviewed merger alternative and another package removal or ceiling ADR.
- Rust-scoped duplicate-body scanning gates live V2 production code. Definite duplicates longer than ten lines must be extracted/declarativized/deleted or receive a narrow generated/performance-isolation waiver; unreliable cross-language/TSX similarity signals remain advisory until labeled.
- `cargo tree -d`, feature unification, artifact-size, idle-RSS/startup, and clean/hot-build reports prevent equivalent dependency stacks or adapter packages from hiding footprint. Default binary and idle RSS target <=1.25x V1, hot rebuild <=1.25x, clean build <=1.5x on the frozen reference machine.
- Root loses V1 composition and adapter lines monotonically after each cutover. A wrapper around unchanged V1 code counts as adapter debt, not deleted code.
- Data footprint reports canonical versus derived bytes, store/file/table counts, graph pack reuse, index generations, cache/sidecar families, and migration amplification. A new representation must state its source lineage, rebuild path, reuse/deduplication, retention, and bytes-at-current/10x scale.
- Reuse is rejected when it would erase domain invariants, add optional-heavy dependencies to common paths, or create high fan-in/public-API instability. Such a decision is recorded as `retain` in `reuse-dispositions.json`, not silently duplicated.

## 12. Strangler migration and mandatory deletion schedule

### 12.1 Anti-corruption adapter contract

Every V1 adapter is registered at creation:

```text
adapter_id, bounded_context, v1_source, v2_target, owner,
created_in_pr, shadow_start_gate, cutover_gate, rollback_dependency,
traffic_counter, mismatch_counter, delete_in_pr, status, waiver_expiry
```

Adapters may translate types/calls/results. They may not add policy, query planning, identity derivation, projection, SQL beyond the V1 repository, or silent fallback. Each invocation emits safe adapter telemetry so unused bridges can be proven removable.

### 12.2 Per-context strangler sequence

1. **Inventory/freeze:** enumerate V1 surface/store/schema/config/error/status behavior and freeze fixtures.
2. **Contract:** land V2 types/ports/catalog definition with no route change.
3. **Import/shadow:** capture/import V1 evidence and execute V2 read/decision path without effects.
4. **Compare:** explain mismatches against pinned watermarks; resolve or explicitly approve intentional differences.
5. **Cut over one effect owner:** route writes/commands to V2, retain V1 read-only data for declared rollback, and record a plan 12 §9 `CutoverReceiptV1` (HMAC-signed with the profile-local catalog key per plan 12 §9).
6. **Cut over reads:** V2 becomes default; no live fallback to stale clients/protocols/names.
7. **Rollback drill:** current binary can restore declared data route without reactivating obsolete client semantics.
8. **Retire:** remove route flag, adapter, direct readers/writers, schema migration code no longer required, config/metrics/docs/tests for obsolete behavior.
9. **Delete/securely archive:** remove stores/artifacts whose plan 12 §14.1 disposition is `deleted` after retention/privacy gates; preserve signed manifests/receipts and minimal redacted fixtures.

### 12.3 Mandatory deletion waves

| Wave | Earliest owning phase/PR | Must be deleted when gate passes |
|---|---|---|
| D0: semantic duplicates | PR 4/8/22A contracts | Duplicate ID derivation, scope enums, capability lists, shared error/status/config constants after callers use canonical types |
| D1: store and capture writes | PR 5–10 | Direct provider/hook/session/LCM/analytics/graph writes outside capture/journal/projectors; obsolete backfill markers after receipts |
| D2: query forks | PR 11–16 | Direct SQL/FTS/graph/ranking in CLI/MCP/dashboard and duplicate session/LCM/memory/code pagination/filter paths |
| D3: policy forks | PR 23 series | V1 hint/routing/retrieval/curation/scheduler/coordination evaluators after shadow/calibration/replay gates |
| D4: application/transport forks | PR 24 series | Business mutations/remediation/store routing in CLI/MCP/HTTP/hooks; hand-maintained schemas and clients |
| D5: legacy dashboard | PR 25–32 | Old per-project shell, bespoke endpoints, duplicated filter/action state after route/deep-link/table/export/accessibility parity |
| D6: V1 live system | PR 33–37 | V1 writers, live readers, route flags, adapters, old tool names/protocols, duplicate stores eligible for retirement, obsolete tests/config/docs |

An adapter cannot survive beyond its `delete_in_pr` merely because it is convenient. Extension requires an ADR, evidence of an unmet rollback/parity obligation, a new bounded expiry that still precedes PR 37, and scorecard visibility. The single program-wide adapter end state, stated identically here, in Section 16, in plan 12, and in the master plan: **PR 37 completes with zero live compatibility adapters; every waiver has an expiry that precedes PR 37; expired waivers block CI.** PR 37 also cannot complete with a live V1 store route or duplicated semantic owner. Enforcement is mechanical, not aspirational: the §12.1 adapter registry is the ledger of record; the §13.2 architecture lint counts call sites per `adapter_id` at HEAD and fails any PR that adds a new call site to a registered adapter (or otherwise increases its count) without a ledger amendment; and every adapter row must link its deletion PR before its wave gate closes.

### 12.4 Reconciliation workflows before deletion

For split identity/store/session/graph cases:

- normalize canonical platform paths, reject unsupported/open holders, reserve every source/destination writer, freeze both SQLite families, and capture a signed inventory/watermark (HMAC with the plan 12 §9 profile-local catalog key, `key_id` recorded);
- create and restore-probe an independent immutable backup of every conflicting source before staging; one successful backup never covers another source;
- compute a deterministic confirmation over both source manifests, policy/config/catalog versions, target, table dispositions, remapped-edge digest, collisions, backups, and intended marker/registry update, then revalidate it under the same locks immediately before publication;
- compute entity/observation/projection overlap by privacy-domain-keyed source fingerprints and proven aliases;
- classify every table/index/trigger/sidecar and record as merge/rebuild/reject plus unique, duplicate, conflicting, corrupt, unavailable, secret-flagged, or unsupported; preserve remapped LCM summary/source edges explicitly;
- inspect merge/link/keep-separate effects without content disclosure;
- append/import idempotently into canonical evidence, never copy projection rows as authority;
- rebuild projections/representations;
- compare counts, hashes, coverage, retrieval anchors, and representative queries;
- checkpoint restartable ledger/staging states at every durable boundary; status/doctor emits the exact resume/recover action;
- publish marker plus registry route atomically only after exhaustive verification and emit a reconciliation receipt;
- retain old store read-only for the bounded rollback/evidence window;
- securely retire WAL/temp/cache/backups as required by plan 18.

## 13. Convergence scorecard and architecture tests

### 13.1 Scorecard metrics

| Metric | Definition | V2-default target |
|---|---|---|
| Canonical ownership coverage | Inventoried concepts/effects with exactly one declared owner | 100% |
| Duplicate authority count | Stores/tables/state machines simultaneously treated as canonical for one concept | 0 |
| Unowned store/table count | Persisted structures without owner/migration/retention classification | 0 |
| Direct canonical writers | Call sites outside capture/store/projector/application ownership | 0 |
| Scope resolver implementations | Independent public identity/scope resolution paths | 1 |
| Query semantic implementations | Public query paths bypassing `TraceQueryV1`/owned facades | 0 |
| Policy decision implementations | Live ad hoc evaluators outside policy bundles | 0 |
| Redaction entry implementations | Persistence/exposure paths bypassing mandatory sanitizer | 0 |
| Capability coverage | Public actions with catalog ID and application handler | 100% |
| Transport conformance | Capability fixtures semantically identical across supported transports | 100% |
| Generated contract drift | Uncommitted or conflicting generated output | 0 |
| Adapter burn-down | Temporary adapters past deletion PR/expiry | 0 |
| V1 traffic after context cutover | Calls to cut-over V1 path outside explicit rollback drill | 0 |
| Typed-ID boundary coverage | Public/store interfaces using canonical ID newtypes | 100% |
| Error/status/config parity | Registered variants with mappings on all supported surfaces | 100% |
| Dependency cycles/forbidden imports | Violations in workspace/module graph | 0 |
| Complexity debt | Non-waived hard file/function/complexity violations introduced by V2 | 0 |
| Rust package count | Published/workspace Rust packages including root and official client | <=11; no package for root-only adapters |
| Negative-code parity | Cut-over parity lanes whose handwritten V2 is not smaller than retired V1+adapter code | 0 non-waived |
| Definite duplicate bodies | Live V2 production pairs >10 lines classified definite by the Rust-scoped labeled scanner | 0 non-waived |
| Infrastructure engine count | Unregistered registry/encoder/projector/operation/host-install/extractor-driver/render/page/problem engines | 0 |
| Generated binding coverage | Mechanical public bindings/schemas/docs emitted from registered manifests | 100%; handwritten business behavior excluded |
| Dependency and artifact footprint | Equivalent dependency stacks, unjustified features, or root-only published adapter artifacts | 0 |
| Runtime/build footprint | Default binary/RSS/hot-build/clean-build versus frozen V1 | <=1.25x / <=1.25x / <=1.5x |
| Replayability | Policy/query/capture cases with pinned artifacts and declared substitutions | 100% for supported exact paths |
| Coverage truth | Responses/status that omit required partial/stale/unknown declarations | 0 known cases |
| Hook budget conformance | Hook points meeting plan 07's canonical-path notification/prompt-evaluation p95 budgets | 100% |
| Projector rebuild determinism | Registered projections passing the §13.2 rebuild-determinism test for pinned observation ranges | 100% |

Scores are published per PR and as trends. A high aggregate score cannot mask a security, durability, identity, or silent-data-loss violation; critical invariants are hard gates.

Every metric names an automated detector in `convergence-scorecard.json`; a metric without one cannot be reported green. Judgment-shaped metrics get explicit procedures: duplicate authority count is computed from `stores.json`/`tables.json` owner analysis (two structures declaring canonical ownership of one inventoried concept); policy decision implementations from the `semantic-implementations.json` source scan for evaluator entry points outside registered policy bundles; coverage truth from transport-conformance fixtures asserting the required partial/stale/unknown declarations plus a known-case ledger — a newly discovered undeclared-coverage case reopens the metric.

### 13.2 Architecture tests

Add deterministic tests/tools for:

- workspace DAG and forbidden crate imports;
- source scans limiting SQL to store/V1 adapters and route/query semantics to owners;
- compile-fail tests preventing raw `String` IDs/content at protected boundaries;
- exactly one canonical ID encoder and `ScopeSelectorV2` resolver entry;
- capability/use-case/transport/SDK/dashboard bijection;
- generated OpenAPI/JSON Schema/MCP/CLI/SDK/UI drift;
- error/status/config mapping exhaustiveness;
- adapter ledger completeness, waiver expiry preceding PR 37, per-`adapter_id` call-site counts at HEAD (a PR that adds a call site to a registered adapter or increases its count without a ledger amendment fails), traffic, and deletion-PR link;
- projection registry uniqueness and rebuild determinism;
- schema compatibility/migration fixtures;
- public replay result determinism for pinned inputs;
- privacy sink/canary coverage for every store/index/cache/log/output/fixture/export;
- semantic conformance across application, CLI, MCP, HTTP, SDKs, hooks, and dashboard client;
- split-store identity reconciliation inspect/plan/start/recover/idempotency;
- cross-repo/worktree/ref scope and graph/search routing;
- file/function/complexity/public-API/dependency budget deltas;
- architecture-manifest drift, package ceiling/admission, generated-versus-handwritten lines, negative-code disposition, duplicate-body clusters, dependency duplication/features, table/worker/artifact counts, binary/RSS/startup/build and data-footprint deltas;
- exactly one registry substrate, canonical encoder/digest kernel, projection runtime, operation substrate, hermetic experiment harness, host installer, extractor driver, graph/timeline/metric visualization envelope, thin linked `WorkspaceSlotFrame` plus renderer capability registry, page/problem envelope, and presentation renderer.

### 13.3 Architecture observatory

Expose a read-only `Architecture`/`Convergence` view in Observatory and CLI/API:

- crate/module DAG and forbidden-edge findings;
- owner map from capability to use case to query/policy/repository/projection;
- store/shard/identity route map with conflicts and coverage;
- projection lag/version/watermarks;
- generated-contract digest parity;
- adapter burn-down and live traffic;
- package/module/dependency graph, reuse-disposition clusters, negative-code ledger, retired/live path counts, schema/worker/artifact footprint, binary/RSS/startup/build/data trends, and every active waiver/expiry;
- complexity and public-surface trends;
- reconciliation jobs/receipts and blockers;
- exact retrieval anchors to safe evidence and plan 14 `FM-###` failure rows.

This view cannot expose private data, raw SQL, secret candidates, or filesystem details outside the caller’s access scope.

## 14. Incremental implementation slices

These slices are program gates mapped into the master plan’s PRs, not a competing PR numbering scheme.

### C0 — Phase 0 architecture inventory and ownership lock (`PR 1`, `PR 3`)

- Generate the inventories in Section 2.3 from the accepted master base.
- Author and lock checked `architecture-boundaries.toml`, then generate its DAG/owner/release/deletion reports; record package-admission/merger decisions, the <=11-package ceiling, and the root-private hook/presentation/API/remote-Brain-transport module boundaries.
- Add ADRs for canonical planes, ownership, DAG, config/error/status governance, shared mechanism map, extension tiers, complexity/negative-code/footprint budgets, and adapter expiry.
- Baseline convergence scorecard and historical failure links (plan 14 `FM-###` row IDs).
- Freeze representative semantic parity fixtures without private content.
- Import #425's table-disposition/collision/canonical-path/holder/reservation/dual-backup/confirmation/ledger/remapped-edge/marker/doctor inventory as the V1 reconciliation seam and assign each behavior one V2 owner and deletion gate.
- Gate: every V1 surface/store/implementation has owner, target, disposition, and retrieval anchor.

### C1 — Pure canonical contracts (`PR 4`, `PR 4A`)

- Land domain IDs, scope, evidence, time, safe-content, error/status/config primitives.
- Land capability/use-case/projection/SPI registry shapes and architecture compile-fail tests.
- Build a read-only V1-backed vertical view through adapters; no new V1 behavior.
- Gate: contracts contain no transport/store/provider dependencies.

### C2 — One evidence and storage path (`PR 5–10`)

- Land catalog/activity/project/graph/blob stores, observation journal/outbox, capture registry, mandatory sanitizer, identity allocation, and deterministic projectors.
- Redirect one provider/session/tool/subagent vertical slice end to end.
- Delete its direct write paths as soon as rollback no longer requires them.
- Gate: acknowledged input is neither lost nor written to competing authority under crash/duplicate/late/disk-full tests.

### C3 — One scope/query/search/graph path (`PR 8A`, `PR 11–16`)

- Land `ScopeSelectorV2`, resolve once, `TraceQueryV1`, federated planner/cursors, lexical baseline, evaluation harness, graph/time operators, and all-scope aggregates.
- Route one CLI/MCP/HTTP/dashboard investigation through it.
- Delete corresponding direct SQL/FTS/graph paths.
- Gate: heterogeneous multi-repository and split-worktree fixtures resolve without manual store choreography, implicit provider partitioning, or required live checkouts, with stable routing and truthful partial coverage. The frozen Rspack/Rsbuild/React Router fixture remains one named non-authoritative case.

### C4 — Reconciled domain projections (`PR 17–22`)

- Add agent/session/Turn, work claim, code/lineage, Git/delivery, cross-repo, knowledge, automation/skill, accounting/observability projections.
- Prove canonical entity/relation/time primitives support graph-of-graphs without a generic untyped graph blob.
- Gate: Causal Loom vertical slice follows source -> Turn -> tools -> subagents -> code -> Git/PR -> outcome with stable anchors.

### C5 — One capability and policy runtime (`PR 22A`, `PR 23 series`)

- Generate all public bindings from the capability catalog.
- Add the pure `tracedecay-tool-catalog::host_bundles` compiler over that same catalog/`HostIntegrationManifestV1`; generate host package/component trees and capability/difference/conformance artifacts without host I/O or another package.
- Move hints/retrieval/correlation/coordination/curation/scheduler/memory decisions into versioned pure evaluators.
- Run shadow/calibration/replay gates; delete replaced condition stacks.
- Gate: live and lab evaluations share code, but labs cannot apply effects or pollute analytics.

### C6 — One application layer and official interface (`PR 24 series`)

- Move public use cases, remediation, jobs, status, config, access, idempotency, and audit into application handlers.
- Bind CLI, MCP, HTTP/SSE, SDKs, and hooks as thin adapters.
- Bind root-private `v2::host_deploy` to application-owned integration operations and compiler-resolved signed artifacts; it owns narrow host I/O/compensation only and cannot reinterpret host-bundle semantics.
- Run semantic transport conformance and current-version handshake tests.
- Gate: no public transport owns SQL, scope resolution, ranking, policy, or business mutation.

### C7 — One product (`PR 25–32`)

- Build Brain/All, Explorer, Causal Loom, domain workspaces, graph lenses, labs, and Observatory over generated client/view models.
- Expose Convergence Observatory and adapter/reconciliation status.
- Remove bespoke frontend data/behavior paths as parity slices land.
- Gate: project view is a scoped zoom of one system, not a separate product; table/export/accessibility parity passes.

### C8 — Backfill, reconcile, cut over (`PR 33–36`)

- Run resumable evidence imports, identity/store reconciliations, projection rebuilds, privacy rescans, and shadow comparisons.
- Require the #425-derived dual-nonempty-store matrix: canonical macOS/Linux/Windows paths, holder/reservation races, every crash checkpoint, one-of-two backup failure, confirmation drift, table collision/rebuild/reject, remapped LCM source edges, verification mismatch, and atomic marker/registry publication.
- Cut bounded contexts one effect owner at a time with signed receipts and rollback drills.
- Reject stale clients/obsolete protocols/names before store use.
- Gate: no unexplained parity gap, unscanned private descendant, or split authoritative identity remains.

### C9 — Delete V1 and close entropy budget (`PR 37`)

- Remove TraceDecay-owned V1 routes/adapters/writers/readers/stores eligible for retirement, obsolete flags/config/docs/tests/dependencies, and expired waivers. External host stores and Hermes-owned transcripts/board databases/caches/backups are never deletion targets.
- In PR 37K, remove copied per-host installer/config/manifest fragments after generated-bundle parity; preserve every foreign cache/config/backup/unmanaged package and any path without receipt ownership.
- Regenerate inventory and scorecard from the final tree/runtime manifests.
- Prove every parity lane is net-negative handwritten code, all named duplicate/infrastructure clusters are deleted or narrowly waived, package/artifact/dependency/table/worker/runtime/build/data budgets pass, and root-only adapters never became published packages.
- Archive only minimal redacted evidence, manifests, benchmark/calibration/parity/privacy/reconciliation/rollback receipts.
- Gate: every scorecard target passes and no active use case depends on V1 code or a compatibility adapter.

## 15. Risks and mitigations

### Native semantic code-search convergence

FastEmbed is the single optional V2 native embedding runtime, isolated in root-private `src/v2/native_semantic_runtime`; no new package is admitted. Plan 25 owns eligible code-document/chunk semantics and input digests, plan 04 owns incremental scheduling, plan 02 owns immutable vector generations and atomic activation, plan 05 owns retrieval/ranking, and root owns only runtime adaptation. PR 14E must extend architecture lint with a repository-wide dependency/import exclusivity rule: `fastembed` is legal only in the root-private adapter, while direct `ort`, Nomic, alternate embedding runtimes, and duplicate vector/store/scheduler/query implementations are rejected everywhere. The lint includes negative fixtures for an illegal root-module `fastembed` import and direct runtime imports; no plan may claim this boundary is enforced before those fixtures pass.

Convergence requires one manifest vocabulary carrying model/revision/artifact, tokenizer, runtime ABI, dimension, metric, normalization, formatter/chunk, privacy/key, source/input digest, and generation pins. Unchanged compatible rows reuse; incompatibility rebuilds; generations never mix. The February 2026 direct-runtime designs remain historical provenance but cannot govern V2. Holographic-memory algebra remains a separate memory representation and retrieval signal, never an embedding backend, compatible vector generation, or migration source for code semantic search.

| Risk | Mitigation/gate |
|---|---|
| “Canonical” crates become monoliths | Bounded contexts, module/file budgets, consumer-owned ports, owner maps, architecture reviews |
| Shared abstractions erase domain meaning | Shared entity/evidence primitives plus typed domain projections/operators; reject generic map/blob APIs |
| Over-generation creates unreadable APIs | Generate mechanical bindings only; keep reviewed application contracts and thin idiomatic helpers |
| Extension points freeze too early | Keep internal until two implementations; version manifests; require conformance and explicit stability status |
| Plugin sandbox is falsely trusted | Capability-deny default, WASM/subprocess isolation, resource limits, no-content findings, threat-model tests |
| Embedded shards create distributed-system bugs | Local transaction boundaries, journal/outbox, vector watermarks, partial-state responses, reconciliation receipts |
| Strangler doubles complexity indefinitely | Adapter ledger, traffic metrics, delete-by PR, CI expiry, PR 37 zero-adapter gate |
| Parity preserves known bad behavior | Historical failures (plan 14 `FM-###` rows) classify intended fix vs parity; ADR records deliberate semantic changes and new fixtures |
| Reconciliation merges unrelated identities | Evidence-backed candidate model, preview, human confirmation for ambiguity, reversible publish, preserved sources |
| Reconciliation loses unique evidence | Append/import observations, manifests/hashes/counts/anchors, rebuild projections, idempotent resume, rollback drill |
| Scope convenience reintroduces implicit routing | Resolve once; explicit selectors bypass CWD; response echoes resolved scope; conformance corpus |
| Query unification becomes slow | Capability-based planner, shard pruning, immutable representations, budgets, benchmark decomposition |
| Policy centralization becomes a god engine | Independent pure evaluators under bundle registry; application owns effects; no I/O in policy |
| Redaction unification destroys useful evidence | Typed classification, marker/quarantine policy, false-positive adjudication, receipts, synthetic regression corpus |
| Complexity metrics encourage superficial splitting | Pair numeric budgets with responsibility/ownership review and prohibit continuation/helper dumping grounds |
| Fewer packages recreate the root monolith | Collapse only root-only adapters; preserve core dependency firewalls, enforce private module import lints, and keep independent contract/performance/conformance tests |
| Reuse creates a generic god kernel | Reuse stable mechanics only; owners retain domain state/admission, ports remain consumer-owned, and fan-in/public-item/downstream-rebuild budgets gate the domain/application facades |
| Generated code hides generator complexity | Report generated and handwritten lines separately, gate generator size/time, diff generated artifacts, and never count generated bulk as negative code |
| Open PR/master changes invalidate inventory | Refresh base and open PR state before each slice; manifests pin commit/catalog/schema digests |

## 16. Definition of done

- [ ] Every persisted concept, state machine, public capability, configuration value, status fact, error, and effect has exactly one canonical owner.
- [ ] Every supported source enters one observation/sanitization/journal path; no acknowledged record is silently lost or written as competing authority.
- [ ] Sessions and LCM reconcile as activity plus context lineage with one identity/retrieval-anchor model.
- [ ] One identity/scope resolver handles profile/repository/project/checkout/worktree/ref/session/agent/all-system scope on every surface.
- [ ] Split legacy/selected stores are discoverable, previewable, reconcilable, verifiable, and safely retireable through typed application workflows.
- [ ] #425/V2 reconciliation freezes and backs up both sources, preserves remapped LCM source edges, accounts for every table/collision, resumes every ledger state, revalidates confirmation under locks, and publishes marker/registry state only after exhaustive proof; doctor exposes one executable recovery action.
- [ ] One query/search/graph plane serves CLI, MCP, API, SDKs, dashboard, policy, and labs with pinned scope, coverage, freshness, explain, and anchors.
- [ ] One policy/replay plane evaluates hints, retrieval, coordination, curation, memory, diagnostics, and scheduling without hidden I/O or effects.
- [ ] One capability catalog generates every public binding and discovery surface; drift tests pass.
- [ ] One application layer owns commands, queries, remediation, status, config, jobs, idempotency, access, and audit.
- [ ] CLI, MCP, HTTP/SSE, SDKs, hooks, and dashboard are semantically conformant thin adapters.
- [ ] Redaction is one mandatory typed boundary; no optional/provider/memory/output-specific path can bypass it.
- [ ] Extension SPIs are bounded, versioned, budgeted, provenance-rich, sandboxed by trust tier, and incapable of bypassing scope/privacy/effect rules.
- [ ] Crate dependency DAG has zero cycles and zero non-waived forbidden edges.
- [ ] Workspace contains at most 11 Rust packages including the root package and official client crate; root emits thin `tracedecay` plus private `tracedecayd` binaries without another package. Hook, presentation, API, service-identity/local-transport, host-deployment, and remote-Brain-transport adapters are private root modules with independent lint/test boundaries and no separately published artifacts.
- [ ] File/function/complexity/public-API budgets have no non-waived V2-default violations.
- [ ] Every parity-replacement lane is net-negative handwritten code; generated output is separately accounted; default binary/RSS/hot/clean-build gates pass.
- [ ] The reuse-disposition ledger closes the current host-installer, extractor, registry, query/ranking, operation, projection, rendering/envelope, dashboard-client, and conformance-test clusters with no unregistered infrastructure engine or definite live duplicate body >10 lines.
- [ ] PR 37 completes with zero live compatibility adapters; every waiver has an expiry that precedes PR 37; expired waivers block CI. Each retired adapter's ledger row links its deletion PR, and any earlier bounded rollback obligation was discharged before PR 37, never carried past it.
- [ ] Generated schema/catalog/client/docs artifacts are reproducible, privacy-scanned, and current with the binary handshake.
- [ ] Convergence scorecard reaches every hard target; critical privacy/durability/identity/coverage gates cannot be averaged away.
- [ ] Brain/All, Explorer, Causal Loom, graphs, workspaces, labs, and Observatory all expose the same reconciled system rather than separate stores/products.
- [ ] Final inventory contains no unowned store/table/path, duplicate authority, obsolete protocol/name, or unexplained historical failure gap.

## 17. Implementation handoff rule

Before implementing any slice, the lead must refresh master/open-PR state, regenerate the relevant inventory subset, resolve the research/failure/privacy/convergence anchors from plans 13–19, identify the exact owner and adapter/deletion rows, and add the slice’s scorecard delta to the PR description. A change that creates a second semantic implementation without a registered adapter and deletion PR is incomplete even if its local tests pass.

## 18. Accepted-base refresh delta (audit 29 / packet 30)

The convergence map preserves one canonical daemon-owned physical writer per
store (`f18f0f14`). Deferred live-fact reclamation (PR #455) requires an
explicit periodic exclusive-maintenance cadence in the maintenance-owner
boundary. See
[`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md)
§5, §7.2 and FM-164.
