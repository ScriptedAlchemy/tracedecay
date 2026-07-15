# TraceDecay V2 CLI, MCP, Tool Surface, and Output Unification Plan

**Plan 32 integration:** project its generated CLI, compact `workflow_run|workflow_get|workflow_control` MCP tools, authenticated paged-history resource, Markdown/JSON views, progress/problems/cursors, and plugin recipes from one catalog. MCP resume belongs only to `workflow_control`; CLI name execution requires explicit scope plus version policy, while exact version IDs remain canonical. No surface invents workflow semantics or returns giant raw history.

> **For agentic workers:** implement this plan only inside the existing V2 program. Do not create a parallel command system, renderer stack, scope resolver, error model, or configuration registry.

**Goal:** Replace TraceDecay's independently evolved CLI commands and hand-written MCP protocol/tool stack, routing allowlists, output switches, raw-JSON renderers, pagination conventions, response truncation, help text, and compatibility aliases with one generated semantic surface and one first-class MCP adapter. Skills plus CLI remain the universal baseline; optional MCP exposes only a generated immutable role profile. Every current command, tool, resource, protocol method, and host behavior receives an explicit keep/replace/remove disposition; every surviving binding invokes one application use case and renders one typed result consistently.

**Architecture:** [`08-tool-catalog-crate.md`](08-tool-catalog-crate.md) owns stable capabilities, use cases, binding IDs, effect metadata, generated surface definitions, host component sets/install scopes, and immutable MCP exposure profiles. [`09-application-crate.md`](09-application-crate.md) owns execution and canonical `ApplicationResponse<T>` views. A small pure root `v2::presentation` module converts only sealed typed views into a transport-neutral document model and renders Markdown or terminal text; canonical JSON serializes the same view without passing through the document model. It remains private because CLI and MCP are both root adapters and snapshots/conformance are test consumers, not independent production packages. Generated CLI and MCP adapters resolve the same [`ScopeSelectorV2`](16-cross-project-repository-worktree-scope.md), call the same application port, map the same errors, and apply catalog-declared output, pagination, privacy, and budget policy. One root MCP adapter/binary uses the official Rust MCP SDK behind a pinned protocol/conformance boundary, owns lifecycle/session/framing only, and projects tools, resources, prompts, completion, notifications, and task support from the connection-pinned profile. The three logical registration names are trust-boundary façades over that adapter, not separate services. HTTP/SDK JSON, NDJSON, and SSE remain owned by plans [`10`](10-api-crate.md) and [`17`](17-official-public-api-and-sdks.md); MCP Streamable HTTP is a distinct protocol endpoint, not a REST wrapper.

**Normative dependencies:** [`01-domain-crate.md`](01-domain-crate.md), [`05-query-crate.md`](05-query-crate.md), [`08-tool-catalog-crate.md`](08-tool-catalog-crate.md), [`09-application-crate.md`](09-application-crate.md), [`10-api-crate.md`](10-api-crate.md), [`11-dashboard-frontend.md`](11-dashboard-frontend.md), [`12-root-compatibility-migration.md`](12-root-compatibility-migration.md), [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md), [`17-official-public-api-and-sdks.md`](17-official-public-api-and-sdks.md), [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md), [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md), [`20-configuration-control-plane.md`](20-configuration-control-plane.md), [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md), [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md), [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md), and [`26-observability-accounting-and-usage.md`](26-observability-accounting-and-usage.md).

**Normative MCP baseline (refreshed 2026-07-10):** the current stable revision is [`2025-11-25`](https://modelcontextprotocol.io/docs/learn/versioning). Implementation follows the official [lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle), [tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools), [resources](https://modelcontextprotocol.io/specification/2025-11-25/server/resources), [prompts](https://modelcontextprotocol.io/specification/2025-11-25/server/prompts), [completion](https://modelcontextprotocol.io/specification/2025-11-25/server/utilities/completion), [progress](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/progress), [cancellation](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/cancellation), [transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports), and [authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization) contracts. The adapter uses the [official Rust SDK](https://rust.sdk.modelcontextprotocol.io/) at an exactly pinned, conformance-tested release. The implementation PR refreshes the current stable revision; a draft revision never changes a release contract silently.

---

## 1. Contract lock

1. A user intent maps to one `UseCaseId`; a concrete exposure maps to one `BindingId`. CLI, MCP, HTTP, SDK, dashboard, hook, skill, and automation names are bindings, never separate implementations.
2. The capability catalog is the sole source of names, descriptions, aliases, parameter schemas, defaults, scope requirements, effect class, auth grants, output formats, pagination, budgets, examples, availability, lifecycle, and replacement instructions.
3. The application layer returns a sealed typed semantic view. CLI/MCP renderers cannot query stores, infer missing fields, patch labels, reinterpret errors, or traverse raw `serde_json::Value`.
4. MCP text content defaults to compact Markdown while current-protocol tool results always carry schema-valid canonical `structuredContent`; explicit `format=json` controls the text representation for clients that need a JSON echo. CLI human output defaults to deterministic terminal text and machine callers opt into canonical JSON explicitly. JSON never means “the JSON-RPC wrapper containing a string containing JSON.”
5. Markdown, terminal text, table rows, JSON, NDJSON, and dashboard components derive from the same typed view and field descriptors. A renderer cannot silently drop a result, coverage item, active marker, truncation state, or retry instruction.
6. `ScopeSelectorV2` and the matching pinned `ScopeResolutionV2` are the only scope contracts. CWD may seed an explicit selector only where the catalog declares that default; it never resolves ambiguity by first match.
7. Repository, project, checkout, worktree, branch/ref, snapshot, graph generation, profile, provider, host, session, workflow, and agent filters retain distinct typed identities. `path`, `project`, `project_path`, `project_root`, `root`, and `cwd` do not remain competing semantic selectors.
8. Every mutation declares one execution mode: direct idempotent commit, explicitly confirmed destructive command, autonomous policy effect, resumable workflow, or internal host lifecycle event. There is no universal preview/apply abstraction.
9. Configuration edits use direct validate-and-save with optimistic concurrency. Every non-secret setting, including redactor/privacy/detector configuration, is navigable in Brain Settings and generated `tracedecay config` commands as required by plan 20.
10. Curation is fully autonomous. No V2 CLI command, MCP tool, HTTP route, dashboard action, skill, or generated client exposes per-item preview, approve, reject, apply, install, promote, or rollback for memory/fact curation, session reflection, skill evolution, or related self-improvement.
11. Destructive system operations outside curation may require explicit confirmation and may expose recovery/compensation. This does not create a curation approval queue.
12. Collection results use authenticated opaque cursors and deterministic ordering. A boolean `truncated` plus an unrecoverable prefix is not pagination.
13. Transport-size truncation is always explicit and recoverable through a scoped typed retrieval anchor, or the operation fails with a safe budget problem. No handler invents a private compaction envelope.
14. Success, partial success, empty-complete, empty-incomplete, unavailable, denied, ambiguous, stale, redacted, conflict, pending, and failed are distinct typed states across all surfaces.
15. Safe rendering is mandatory after sanitization and authorization. ANSI, Markdown, terminal controls, paths, labels, errors, examples, response handles, and generated docs all pass plan 18's sink firewall.
16. Current aliases and V1 behavior exist only in frozen inventory and the differential harness after their declared cutoff. There is no permanent dual namespace or silent behavior shim.
17. Generated inventories and conformance fixtures cover every command path and every tool definition, including hidden commands, conditional tools, aliases, defaults, routing-only arguments, and unavailable bindings.
18. MCP lifecycle is a connection state machine. `initialize` is first, protocol/capabilities are negotiated, `notifications/initialized` gates operation, and shutdown/drain/reconnect cannot be implemented as handler-local no-ops.
19. MCP primitive choice follows interaction ownership: tools are model-controlled operations, resources are application-controlled context, prompts are user-controlled recipes, and completion applies only to prompt/resource-template arguments. A convenience cannot be exposed as the wrong primitive merely because tools are widely supported.
20. MCP semantic parity with CLI does not imply wire identity. JSON-RPC errors versus tool execution errors, `structuredContent`, content blocks/resource links, progress/cancellation, list notifications, subscriptions, task augmentation, stdout/stderr, and CLI exit codes retain their native transport contracts.
21. An advertised MCP capability is a tested promise. The server never advertises `listChanged`, subscription, logging, prompts, completion, sampling, elicitation, or task support unless the corresponding send/receive path, authorization boundary, and host conformance fixture exist.
22. Plan 24 domain work items/attempts and MCP protocol tasks are distinct identities. MCP task augmentation is only an asynchronous execution envelope around an `OperationRef`; it never aliases `WorkItemId` or `ExecutionAttemptId`.
23. No V2 MCP resource teaches callers to query an internal database or exposes a physical schema as a supported product API. Durable evidence resolves through typed resources/retrieval anchors and application use cases.
24. Skills plus generated CLI are the complete portable baseline. MCP is an optional component selected in `HostInstallSetV1`; omitting it cannot remove a semantic capability from help, hints, skills, CLI, HTTP, or the official API. `HostInstallModeV1` is legacy migration input only and never a current configuration or execution branch.
25. There is one MCP implementation and exactly three generated logical registrations: `tracedecay-context`, `tracedecay-work`, and explicitly opted-in `tracedecay-operator`. They share binary, daemon, catalog, application, presentation, lifecycle, and conformance code.
26. A connection pins one explicit `McpSurfaceProfileV1` whose fully materialized `BindingId` set contains no glob. Visibility is `profile ∩ negotiated host features ∩ grant ceiling ∩ current authorization`; profile membership and effect ceiling cannot change until reconnect.
27. `listChanged` is not per-turn progressive disclosure. It reports a real catalog/availability/authorization change within the pinned profile. Native deferred tool search may lazily load definitions already in the profile, but eager-list hosts and skills-only hosts remain first-class conformance targets.
28. No MCP or CLI god tool accepts an arbitrary hidden `BindingId` plus untyped arguments. Discovery returns the exact supported binding/command/API recipe; it never bypasses per-use-case schema, annotations, grants, profile ceilings, or audit.
29. Complex plan/task edits use exactly `task_graph.edit_bundles.export|get|validate|diff|rebase|submit|delete`. The frontmatter-Markdown bundle is protected temporary staging compiled back into canonical task commands, never a second task store or writable resource.
30. Only the `tracedecay-work` `orchestrator` profile exposes edit-bundle mutations. Large MCP bundle results use authorized resource links; diagnostics default to compact Markdown with the same explicit JSON view available on demand.
31. Worktree lifecycle bindings discover and relate externally created worktrees; TraceDecay exposes no create/provision-worktree binding. Association, ownership provenance, and cleanup grant are distinct. Cleanup inspect is read-shaped evidence; cleanup request is a daemon-only confirmed workflow over an exact worktree identity, CAS versions, blockers, grant, and preview digest. No CLI/MCP client deletes a path or branch.

## 2. Source audit and concrete fragmentation evidence

### 2.1 Evidence path and limitation

The planning probe first called `tracedecay_context` through MCP with the explicit redesign worktree. MCP was degraded because startup project resolution found many projects. The equivalent CLI call with `--project /home/zack/projects/tracedecay` then failed closed on an identity-cutover conflict between preserved selected and legacy shards. No store was changed. The audit therefore used bounded reads of the current worktree's CLI/MCP/renderer sources and the installed 0.0.47 command/tool registries.

This failure is itself a required regression: explicit worktree/project selection must reach one typed ambiguity/identity problem with candidates and a safe consolidation action. MCP and CLI must not print different startup guidance, suggest initialization for an existing split store, or make explicit selection ineffective.

Primary current paths inspected:

- `src/cli.rs`, `src/cli/automation.rs`, and the native command modules;
- `src/tool_command.rs` and `src/tool_command/args.rs`;
- `src/mcp/tools/definitions.rs` and `definitions/session.rs`;
- `src/mcp/tools/handlers/**`, `render.rs`, and `renderers.rs`;
- `src/mcp/response_handles.rs`, `project_route.rs`, and `dispatch_policy.rs`;
- existing V2 plans 08–10 and 12, 16–20.

The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). V2 absorbs ordinary-profile Hermes, proxy-before-store, bounded catalog refresh, fact rank/counters, exact analytics, release, and consolidation/recovery into generated bindings/handshakes/views rather than adapter-local inventories or renderers.

Merged #425 requires one cataloged offline split-store-consolidation workflow, not transport-local migration flags: status/plan, deterministic confirmation challenge, start/resume/status/cancel-before-cutover, verification report, and recovery. CLI/MCP/API render the same typed view for canonical platform identities, path-plus-file/inode holder/reservation coverage, dual backup receipts, restartable ledger/staging, row/payload/LCM/fact/feedback dispositions, remapped-edge verification, and proof-gated cutover. Ordinary output never reveals holder command lines, unauthorized raw paths, backup secrets, confirmation material, or quarantined content; no surface can skip verification or silently initialize/select a store.

### 2.2 Current registry drift

The checked-out source constructs 104 MCP definitions before host capability filtering. `ast_grep_rewrite` is conditionally removed, so a matching source build exposes 103 or 104. The installed `tracedecay 0.0.47` reports 103 and includes `ast_grep_search`/`ast_grep_rewrite` but not source-defined `move_symbol`. Plan 08's older human baseline lists 102 and omits both `ast_grep_search` and `move_symbol` while already requiring a refresh for merged PR #414.

This proves that counts, source arrays, runtime registration, category output, installed binaries, help, plans, and release state can disagree. V2 inventories therefore record:

- source commit and binary version;
- full pre-filter definition set;
- installed/advertised set plus unavailability reasons;
- host capability probe digest;
- handler/renderer/route binding presence;
- generated catalog/protocol digest;
- exact additions, removals, replacements, and unexplained drift.

A numeric count alone never passes the gate.

### 2.3 Current format and renderer inconsistencies

The current source demonstrates the failure modes this plan eliminates:

- `FORMAT_CAPABLE_TOOL_NAMES` is a hand-maintained 99-name allowlist separate from the 104-definition registry.
- `dsm`, `files`, `sessions_for`, `type_hierarchy`, and `workflows` render through Markdown-capable handlers but do not receive the shared `format` input schema. Some are simultaneously named by the unused `tool_defaults_to_markdown` predicate.
- `tool_defaults_to_markdown` is exported but has no production consumer, so it can disagree with actual handler behavior without changing runtime output.
- all injected `format` descriptions say Markdown is the default, but schema presence and handler routing are independent lists.
- several handlers use dedicated renderers; others feed arbitrary `serde_json::Value` through `generic_md`; past source comments document a real case where `unsafe_patterns` went through the diagnostics renderer and falsely displayed no findings.
- project registry rendering has bespoke code to preserve `projects`, `project_tree`, `summary`, `limit`, `truncated`, and active markers on missing-registry paths; the invariant is not enforced for other result families.
- one global 15,000-character cap is applied after rendering, while LCM handlers have additional contract-specific compaction tiers and can report `compacted_no_handle`.
- response handles are project-root local, expire after 24 hours, and require callers to remember the same project selector. Some paths remain irreversibly truncated when a root is unavailable.
- MCP `format=json` serializes the semantic payload into `content[*].text`; `tracedecay tool --json` instead prints the raw tool result wrapper. Combining the concepts can yield a JSON envelope whose `text` is another JSON string.
- `tracedecay tool --dry-run` is a transport-side parse/validation switch, but edit/use-case schemas may also define semantic `dry_run`; the reserved flag intercepts the name and forces callers to use whole-object JSON to express the semantic field.
- routing keys such as `project_root`, `storage_scope`, `hermes_home`, `response_handle_project_root`, and `cwd` bypass normal schema validation through a separate allowlist.
- profile-scoped LCM and first-touch store tools have additional CLI allowlists that must manually match definitions, daemon behavior, and generated Hermes code.
- native commands use `--json`, `--export json|csv`, `--jsonl`, colored console tables, prose, or no machine format according to local implementation.
- normal result/progress text is inconsistently written to stdout or stderr. Some command modules call `process::exit` directly, while others return `TraceDecayError`.
- `tracedecay status` emits the full ANSI/true-color half-block dashboard even when stdout is piped with `TERM=dumb` and `NO_COLOR=1`; noninteractive agents receive a bitmap-like flood instead of bounded plain or typed status (FM-116).
- at least one invalid output mode (`cost --export <unknown>`) prints an error locally instead of sharing typed validation and a guaranteed nonzero exit.
- limits and ordering are handler-local; many lists return a cap or `truncated` boolean without an opaque resumable cursor.

Every item above becomes a fixture before V2 adapter work begins.

### 2.4 Current MCP protocol fragmentation

The refreshed source audit used the installed 0.0.52 CLI against the explicit redesign worktree, then inspected the indexed MCP symbols. It found a protocol implementation that has outgrown its hand-written shape:

- `src/mcp/server.rs` hard-codes protocol revision `2024-11-05`, returns capabilities without negotiating the request version, and treats `initialized` as a compatibility no-op rather than a connection-state transition.
- `McpMethod` recognizes only initialize, tools list/call, resources list/read, ping/log-level acknowledgement, one private hook notification, and unknown. Prompts, resource templates, completion, resource subscribe/unsubscribe, progress, cancellation, roots-list changes, client responses, sampling, elicitation, and protocol tasks have no first-class dispatch path.
- `logging/setLevel` is acknowledged without maintaining a connection-local severity filter. Update warnings are pushed through a global pending-notification vector, while `tools.listChanged` delivery lives separately in daemon proxy code. Advertised capabilities and actual notification owners can therefore drift.
- `tools/list` and the five static resources are built in server/definition code without protocol pagination. The resources expose a raw SQLite schema and explicitly instruct callers to query internal storage, contradicting the V2 application boundary and the installed CLI skill's no-raw-database guardrail.
- no live tool defines `outputSchema` or returns `structuredContent`; machine JSON is text nested inside the result content. Tool execution errors, semantic failure detection, update notices, staleness banners, metrics, and automation notices are mutated into the result after handlers return.
- the main connection loop reads one line, awaits the complete handler, drains global notifications, then reads again. It cannot receive `notifications/cancelled`, client responses to server requests, or root changes while a long request is executing. Shared-daemon connections reuse engine state but do not have a complete isolated MCP session actor.
- `src/mcp/degraded.rs`, replay transport, daemon handshake interception, server routing, definition construction, dispatch allowlists, renderer selection, project routing, response handles, and analytics each own part of the protocol. The 3,800-line definitions file and 3,200-line server are symptoms of missing generated boundaries, not files to preserve.
- the global 15,000-character renderer cap creates a private `tracedecay_retrieve`/24-hour project-cache protocol instead of returning a typed MCP resource link backed by the canonical retrieval-anchor record.

These findings become frozen source fixtures. The V2 design does not incrementally add more match arms to this stack; it replaces the protocol implementation after differential and host conformance pass.

## 3. Complete current CLI inventory and required disposition

The generated recursive clap inventory is authoritative. The following human matrix is an audit anchor and must remain complete until the V1 cutoff.

| Current path family | Every current path | V2 disposition |
|---|---|---|
| Core index and status | `init`, `sync`, `status`, `list`, `wipe`, `gitignore` | Bind typed project enrollment, capture/index workflow, system status, registry query, confirmed destructive retirement, and configuration use cases. Remove local output/scope logic. |
| Schema-exact CLI tool bridge | `tool`, generated `help` | Keep `tool <current-name>` as a generated CLI-only fallback for skills/MCP-optional hosts, with the same per-use-case schema/effect/grant/scope/auth/output contract as native commands. It cannot accept hidden `BindingId` values or appear as an MCP god tool; catalog help/search/schema remains the discovery surface. |
| Agent integration | `install`/`claude-install`, `reinstall`, `update-plugin`/`update-plugins`, `uninstall`/`claude-uninstall` | Inventory-only deprecated aliases map respectively to `integration install`, `integration repair`, `integration update`, and `integration uninstall` until their generated cutoff. They never appear as current help/MCP/API bindings and perform no work after cutoff. |
| Runtime surfaces | `dashboard`, `serve`, `daemon run`, `daemon install-service`, `daemon uninstall-service`, `daemon restart`, `daemon status` | Bind lifecycle workflows/status; generated auth/effect/progress/output rules. |
| Update lifecycle | `upgrade`, `update`, `channel`, hidden `post-update` | Separate query/config/direct workflow/internal lifecycle bindings. Parent-child lease token remains internal and never appears in public help/output. |
| Accounting | `current-counter`, `reset-counter`, `disable-upload-counter`, `enable-upload-counter`, `cost`, `bench`, `gain`, `monitor` | Consolidate query/config/command/stream use cases; canonical JSON/NDJSON and terminal views replace local tables/export switches. |
| Diagnostics | `doctor`, `lsp servers` | Share typed system status/problem/remediation views; never print a second error taxonomy. |
| Sessions | `sessions ingest`, `sessions search`, `sessions git-backfill`, `sessions unfinished` | Replace ingest/backfill with observable workflows; all reads use canonical session/message/Git scope, cursor, coverage, and result views. |
| Analytics | `analytics diagnostics`, `analytics sync` | Typed query plus import workflow; no output or scope special case. |
| Project registry | `projects list`, `projects search`, `projects context` | Preserve stable empty/missing shapes, safe labels, active state, candidates, cursor, and explicit All/exact scope. Retire old root `list` after parity. |
| Branches | `branch list`, `branch add`, `branch remove`, `branch removeall`, `branch gc`, `branch autotrack status`, `branch autotrack enable`, `branch autotrack disable` | Recast reads, configuration, enrollment, and confirmed retirement as distinct use cases; `removeall` gets a normalized name before cutoff. |
| Memory | `memory status`, `memory curate` | Keep status. Remove manual curate preview/apply/LLM-ops surface; autonomous curation exposes policy, health, runs, decisions, outcomes, pin/protect/exclude, feedback, pause/resume, and run-now only. |
| Automation config | `automation config get`, `explain`, `enable`, `disable`, `set` | Replace with generated plan-20 `config` use cases; automation module is one navigable branch of the complete registry. |
| Automation runs | `automation run memory-curation`, `session-reflection`, `skill-writing`; `automation runs list`, `view`, `artifact` | Remove dry-run/proposal semantics. Keep autonomous run-now and read-only dirty-scope/admission/run/artifact/outcome views with pinned policy/config/eval digests. Run-now never bypasses identical successful/`NoChange` input fencing. |
| Managed skills | `automation skills list`, `view`, `draft`, `update`, `approve`, `disable`, `archive`, `restore`, `install` | Remove per-item authoring/approval/promotion bindings from autonomous evolution. Replace with inventory, history, decisions, outcomes, authority, pin/protect/exclude, health, and feedback. |
| Fact proposals | `automation facts list`, `view`, `apply`, `reject` | Remove proposal queue and item mutations. Replace with autonomous decision/effect history and policy/quality controls. |
| Store migration | `migrate plan`, `export`, `apply`, `verify`, `reconstruct`, `registry-gc`, `rollback`, `cleanup-sources` | Map to typed inventory, export, verified migration workflows, confirmed destructive cleanup, and recovery. Curation autonomy does not remove system migration safety. Rename V1 ceremony terms where the V2 workflow model supersedes them. |
| Hidden extraction | `extract-worker` | Internal host binding only; generated protocol/version handshake and machine-only output. |
| Hidden Claude hooks | Legacy six aliases route only through migration; current generated bindings are `hook-claude-session-start`, `hook-claude-setup`, `hook-claude-instructions-loaded`, `hook-claude-user-prompt-submit`, `hook-claude-user-prompt-expansion`, `hook-claude-message-display`, `hook-claude-pre-tool-use`, `hook-claude-permission-request`, `hook-claude-permission-denied`, `hook-claude-post-tool-use`, `hook-claude-post-tool-use-failure`, `hook-claude-post-tool-batch`, `hook-claude-notification`, `hook-claude-subagent-start`, `hook-claude-subagent-stop`, `hook-claude-task-created`, `hook-claude-task-completed`, `hook-claude-stop`, `hook-claude-stop-failure`, `hook-claude-teammate-idle`, `hook-claude-config-change`, `hook-claude-cwd-changed`, `hook-claude-file-changed`, `hook-claude-worktree-create`, `hook-claude-worktree-remove`, `hook-claude-pre-compact`, `hook-claude-post-compact`, `hook-claude-elicitation`, `hook-claude-elicitation-result`, and `hook-claude-session-end` | Generated from the independent 30-event manifest; normal help/completion still hides all host lifecycle bindings. |
| Hidden Kiro hooks | `hook-kiro-pre-tool-use`, `hook-kiro-prompt-submit`, `hook-kiro-post-tool-use` | Same internal hook contract. |
| Hidden Cursor hooks | `hook-cursor-subagent-start`, `hook-cursor-post-tool-use`, `hook-cursor-before-submit-prompt`, `hook-cursor-pre-compact`, `hook-cursor-after-file-edit`, `hook-cursor-session-start`, `hook-cursor-session-end`, `hook-cursor-after-shell`, `hook-cursor-workspace-open`, `hook-cursor-stop` | Same internal hook contract. |
| Hidden Codex hooks | `hook-codex-session-start`, `hook-codex-subagent-start`, `hook-codex-pre-tool-use`, `hook-codex-permission-request`, `hook-codex-post-tool-use`, `hook-codex-pre-compact`, `hook-codex-post-compact`, `hook-codex-user-prompt-submit`, `hook-codex-subagent-stop`, `hook-codex-stop` | Generated from the independent ten-event Codex manifest; exact event output schema, matcher/input/timeout/platform/trust metadata, and no invented failure binding. |

The Claude event name `hook-claude-worktree-create` is an inbound observation that an external host already created a worktree. It is not a TraceDecay worktree-create command, tool, use case, cleanup grant, or proof of TraceDecay ownership.

All provider hook executables lower to one catalog-generated hidden binding:

```text
tracedecay host-event ingest --host <host-code> --event <event-code> --binding <catalog-binding-id> --stdin
```

It is `InternalHostLifecycle`, absent from normal help, shell completion, MCP, HTTP, SDKs, dashboard actions, and public catalog search. The installer binds only generated closed host/event codes and a release-manifest-bound catalog `HostHookBindingId`; arbitrary binding IDs fail before evaluation. `--stdin` consumes exactly one bounded versioned event envelope and EOF; inline payload flags, positional payload, file/path input, arbitrary event names, multiple envelopes, and trailing bytes are rejected. The adapter authenticates the installed host context, sanitizes before capture, and writes only the catalog-generated host response legal for that exact event: empty, plain context, or schema-valid JSON/exit status as plan 07 specifies. Diagnostics go to version-stamped stderr and never corrupt hook stdout; raw arguments/body are never echoed. Deprecated hidden hook paths remain migration launchers only and are removed host-by-host after receipt parity.

Generated operator/status views expose redacted Codex hook definition/source/trust/effective/skip/overlap/run state and the exact host-native `/hooks` remediation action, but they never expose a generic hook invocation command, command body, path, raw matcher payload, or a way to mark trust. `--dangerously-bypass-hook-trust` is at most an observed ephemeral Codex process fact; TraceDecay does not persist, recommend, or synthesize it as configuration.

Claude status views expose only sanitized definition identity, source/component lifetime, event/matcher/`if` class, handler kind, sync/async/rewake, support/version, managed/disable state, host-dedupe disposition, run/completion/delivery coverage, and owning-source edit guidance. They never copy Claude `/hooks` full command/prompt/URL details, headers, MCP input, args, path placeholders, environment values, or foreign output. The hidden binding renders the exact event-legal response for the pinned stock version; it cannot be called through MCP/HTTP/SDK/dashboard or used to execute an arbitrary event/type.

Plan 24 adds generated V2-only `initiative`, `plan`, `task`, `executor`, `scheduler`, and `task-graph` groups plus audience-filtered executor lifecycle bindings. Task definitions use the existing `saved-view` group and `saved_views.*` operations; no `task-view` command namespace exists. The binding manifest preserves exact commands `work_items.record_attestation`, `work_items.record_review`, `work_items.record_decision`, `work_items.record_exception`, `work_items.handoff`, `work_items.reopen`, `work_items.reverse_transition`, `task_offers.accept|decline|revoke`, `attempts.heartbeat|progress|complete|block`, `context_packets.accept`, `task_notifications.create|update|delete`, and protected `saved_views.share.plan|start|revoke`. Review Markdown/JSON and MCP resources render Plan 24 §4.5A's same sealed lineage/validity/remediation schema, typed anchor availability, readiness digest, cursor, and legal capabilities; compact mode may omit prose but not authority or exclusion reasons. Delayed/stale resources fail closed and no binding exposes an aggregate combined verdict or mutable current-review pointer. `task_offers.accept` is the sole public execution-admission binding; no CLI/MCP alias named `work_items.acquire_lease` exists. Every CLI/MCP/HTTP/SDK/dashboard exposure maps to one catalog use case and sealed task/plan/executor view; compact output always retains canonical IDs/versions, blockers, coverage, packet/lease/route status, anchors, and legal next actions. Fence proofs, credentials, protected logs, and private sibling content never render.

Autonomous-loop observation has exactly three catalog operations and generated CLI/MCP bindings:

| Operation | CLI | MCP tool/resource |
|---|---|---|
| `automation.dirty_scopes.list` | `tracedecay automation dirty-scopes list` | `automation_dirty_scopes_list` tool |
| `automation.admissions.list` | `tracedecay automation admissions list` | `automation_admissions_list` tool |
| `automation.admissions.get` | `tracedecay automation admissions get <admission-id>` | `automation_admissions_get` tool and read-only `tracedecay://automation/admissions/{admission_id}` resource template |

The list operations remain semantic tools; MCP `resources/list` only discovers the addressable receipt template and never substitutes for them. `admissions list --representation receipts|coalesced-skip-episodes` and MCP `representation = receipts|coalesced_skip_episodes` are generated spellings of one request enum, not distinct operations. Exact-receipt and coalesced-episode output comes from one sealed application view: episodes preserve stable anchor, first/last evaluation time, evaluation count, latest policy-evaluation ID, job/scope, exact reason, semantic-input/frontier tuple, next reconsideration, and model/tool/token/cost work avoided. Dirty-scope output places per-shard current, considered, consumed, and included frontiers beside pending delta, unconsumed generation/count/reasons, active-writer/coverage state, and quiet/retry deadlines. Jobs/scheduler/dirty/admission output reuse generic operation, retry directive, policy-health/circuit/pause, privacy-quarantine/reconciliation, and coverage types; renderers cannot infer or rename them. Compact output keeps exact IDs, frontier tuple, reason/state, reconsideration, and coverage.

No `automation skip-episodes`, `automation frontiers`, retry/circuit/quarantine command, MCP alias, or writable resource is generated. The existing run-now binding has no force/ignore-digest option: it may shorten cadence only for an already-dirty scope and still refuses identical successful/`NoChange` input, backoff, open circuit, pause, quarantine, and incomplete coverage. Unchanged/historical inspection routes to the generic experiment family.

Context Scout uses exactly the eleven plan-08/09/22 catalog rows:

```text
reads: scout.status.get; scout.runs.list; scout.runs.get; scout.envelopes.list; scout.envelopes.get; scout.decision.explain; scout.evaluation.get
commands: scout.feedback.record; scout.runtime.pause; scout.runtime.resume; scout.runtime.cancel
```

The generated CLI tree is `tracedecay scout status`, `runs list|show`, `suggestions list|show|explain`, `evaluation show`, `feedback`, `pause`, `resume`, and `cancel`; `suggestions` is presentation over `scout.envelopes.*`, never a new operation family. HTTP binds the same rows under `/api/v2/scout/**`; SDK methods retain canonical semantic names. Reviewed context MCP profiles may expose compact status, addressed pending/history lookup, explanation, evaluation status, and feedback; runtime controls require an operator-capable profile. MCP list/detail output is paged and anchor-bearing and never includes an entire queue or model transcript. No `scout.replay.*` CLI/MCP/HTTP/SDK binding is generated: `experiment fork|create|run|...` with `LabKindV1::Hint` and scout evaluator mode is the sole replay lifecycle.

Research provenance adds catalog operation IDs `research.manifests.list/get/create_version` and the canonical evidence operation family `retrieval_anchors.metadata_batch_get`, `retrieval_anchors.resolve`, and `retrieval_recipes.execute`. Generated bindings may accept `ResearchAnchorId` only for manifest navigation; evidence metadata/resolution/recipe execution accepts canonical `RetrievalAnchorId`/`RetrievalRecipeV1` and never treats safe metadata as payload authority. No CLI, MCP, HTTP, SDK, dashboard, or export binding invents a research-specific resolver or treats a manifest-entry ID as payload authority.

Search evaluation uses only the canonical plan-15 family:

```text
reads: retrieval.{corpus_versions,qrel_versions,candidate_pools,judgments,adjudications,evaluation_reports,profiles}.list|get; generic experiments/experiment_runs/experiment_cells/replay_stages/replay_comparisons/replay_comparison_cells/replay_reductions list|get filtered to LabKindV1::SearchQuality; experiments.evaluator_catalog.get; experiments.draft_from_selection
commands: retrieval.corpus_versions.create|freeze; retrieval.qrel_versions.create|freeze; retrieval.candidate_pools.create; retrieval.judgments.record|supersede; retrieval.adjudications.record; generic experiments.create and experiment_runs.create|cancel|resume|retry|minimize; retrieval.evaluation_reports.publish; experiments.fixtures.promote; retrieval.profiles.publish|activate
```

Each read generates one semantic CLI binding, MCP tool, read-only MCP resource/resource-template where addressable, HTTP/SDK binding, and Search Quality UI view metadata from the same typed result. MCP `resources/list` discovers resources; it never replaces the canonical `.list` use case. Commands generate CLI/MCP/HTTP/SDK/UI actions and no writable resource. No transport may add `eval`, `benchmark`, `golden`, `retrieval.fixtures.*`, or any other alias/use case absent above.

The extractor also records every flag, positional, alias, conflict, required relationship, enum/range, default, env source, hidden state, TTY behavior, stdin/file behavior, color behavior, output family, exit path, effect, and called handler. A command path without a reviewed disposition blocks catalog generation.

## 4. Complete current MCP inventory and required disposition

The current source's pre-capability-filter set is the compatibility anchor. Every name below gets a source definition, advertised-state, handler, typed request/result, renderer, scope, effect, auth, pagination, budget, and migration row.

| Current category | Current source names |
|---|---|
| Always loaded (7) | `search`, `grep`, `context`, `callers`, `status`, `active_project`, `storage_status` |
| Analysis (17) | `circular`, `complexity`, `constructors`, `coupling`, `dead_code`, `distribution`, `doc_coverage`, `field_sites`, `god_class`, `hotspots`, `inheritance_depth`, `largest`, `module_api`, `rank`, `recursion`, `unsafe_patterns`, `unused_imports` |
| Edit (7) | `ast_grep_rewrite`, `insert_at`, `insert_at_symbol`, `move_symbol`, `multi_str_replace`, `replace_symbol`, `str_replace` |
| Git and history (8) | `affected`, `branch_diff`, `branch_list`, `branch_search`, `changelog`, `commit_context`, `diff_context`, `pr_context` |
| Graph (14) | `by_qualified_name`, `call_chain`, `callees`, `callers_for`, `derives`, `file_dependents`, `find_exact_symbol`, `impact`, `implementations`, `impls`, `rename_preview`, `signature`, `similar`, `type_hierarchy` |
| Health (8) | `dependency_depth`, `dsm`, `gini`, `health`, `redundancy`, `runtime`, `test_map`, `test_risk` |
| Information (35) | `analytics`, `ast_grep_search`, `automation_run_artifact_view`, `body`, `config`, `dashboard`, `files`, `hermes_skill_bridge`, `lcm_compress`, `lcm_describe`, `lcm_doctor`, `lcm_expand`, `lcm_expand_query`, `lcm_grep`, `lcm_load_session`, `lcm_preflight`, `lcm_session_boundary`, `lcm_status`, `message_search`, `node`, `outline`, `port_order`, `port_status`, `project_context`, `project_list`, `project_search`, `read`, `retrieve`, `sessions_for`, `signature_search`, `simplify_scan`, `skill_list`, `skill_view`, `todos`, `workflows` |
| Memory and session (5) | `fact_feedback`, `fact_store`, `memory_status`, `session_end`, `session_start` |
| Workflow (3) | `diagnose`, `diagnostics`, `run_affected_tests` |

Current categories are legacy discovery labels, not V2 semantic ownership. PR 22A regeneration may move names or replace overlapping tools, but no current row may disappear without a versioned replacement/removal receipt. Conditional tools remain discoverable as unavailable with the missing host capability instead of silently changing the catalog shape.

The inventory must detect the full set, not only the names printed by `tracedecay tool`. It compares:

1. checked definition constructors;
2. format/scope/availability augmentation;
3. runtime filtered definitions;
4. dispatch match arms;
5. handler functions and semantic error classification;
6. renderer selection;
7. CLI bridge support and help;
8. daemon/profile/project routing;
9. generated provider/plugin schemas;
10. tests and docs.

V2 then places capabilities deliberately instead of publishing everything as a tool:

| MCP primitive | TraceDecay use | Required rule |
|---|---|---|
| Tool | Model-controlled query or command that invokes one application use case | Generated `inputSchema`, tagged `outputSchema`, effect annotations, grant, scope, timeout/cancellation, and optional protocol-task support. |
| Resource | Application-controlled, addressable context or immutable evidence | `tracedecay://v2/...` URI, read authorization, content type/size/retention, annotations, and stable retrieval anchor where durable. Never a raw DB path/schema. |
| Resource template | High-cardinality typed objects such as session timelines, turns, tasks/plans, graph lenses, catalog generations, and retrieval anchors | URI-template variables are typed IDs or safe aliases; list output stays bounded and completion is access-filtered. |
| Prompt | User-controlled investigation recipe such as trace-agent-session, inspect-turn, compare-worktrees, replay-hint-decision, or coordinate-nearby-work | Returns reviewed prompt messages/resource references only; never executes a command, approves curation, or embeds an unauthorized payload. |
| Completion | Ranked suggestions for prompt arguments or resource-template variables | Maximum 100, deterministic, access-filtered, no secret/raw-path echo. It is not a tool-argument completion extension. |

Client features are separate: roots can seed scope candidates; sampling and elicitation are optional server-to-client requests. They are never advertised or invoked without the client's negotiated capability and plan-18/policy authorization.

## 5. Stable semantic identity and generated surface manifests

### 5.1 IDs

Use plan 08's identity model without surface-derived business identity:

`CapabilityId`, `UseCaseId`, `IntentId`, `BindingId`, and `PresentationId` are imported unchanged from plan 08's `id.rs` (using the primitive domain ID vocabulary where plan 08 specifies it). Plan 21 owns presentation descriptors and rendering behavior, not identifier types, constructors, or grammars; it cannot mint a private intent/presentation key.

All five ID kinds follow plan 08 §8's grammar exactly — `capability.<domain>.<noun>`, `usecase.<domain>.<verb-noun>`, `intent.<domain>.<task>`, `binding.<surface>.<stable-name>`, and `presentation.<domain>.<view>` (registered in plan 08's `id.rs`). Capability/use-case storage types remain the plan-01 definitions; versions are separate SemVer fields, and IDs never embed v1/v2 or transport names except BindingId.

One use case may have native CLI, generic CLI bridge, MCP, HTTP, SDK, dashboard, hook, and skill bindings. A binding declares only transport syntax and presentation support. It cannot alter default scope, query semantics, ordering, coverage, effect, or errors.

### 5.2 Canonical generated artifacts

Plan 08 §6's `generated/` filename set is the single canonical artifact home; this plan renders only from those files and adds its surface artifacts to that same set:

```text
generated/                  # canonical home and names: plan 08 §6
├── catalog.json            # capabilities + use cases
├── cli-bindings.json
├── cli-command-tree.json
├── mcp-protocol.json       # pinned revision, method/capability profile, extension metadata
├── mcp-surface-profiles.json # logical registrations, explicit binding sets, install/effect/grant/host/budget ceilings
├── mcp-tools.json          # emitted tool/input/output/annotation/task definitions
├── mcp-resources.json      # resources/templates/subscriptions/list generations
├── mcp-prompts.json        # prompts, arguments, completion eligibility/list generations
├── presentations.json
├── output-formats.json
├── errors-and-exit-codes.json
├── aliases-and-cutoffs.json
├── scope-bindings.json
├── effect-bindings.json
└── parity-matrix.json
```

Earlier drafts of this plan named variants `capability-catalog.json`/`use-cases.json` (the same artifact as `catalog.json`) and `mcp-bindings.json`/`mcp-tool-definitions.json` (the same artifact as `mcp-tools.json`); those variant names are removed — there is exactly one generator (plan 08's catalog-gen) and one filename per artifact. Configuration metadata inside these files comes only from plan 20's `config-registry-v1.json` descriptor manifest consumed by the plan 08 catalog build; this plan emits no config surface metadata of its own.

Each CLI/MCP row records:

- stable IDs and semantic version;
- current name, category/path, aliases, replacement, introduced/deprecated/cutoff protocol;
- typed request/result schema refs and lossless field mapping;
- required/default/range/enum/units and stdin/file affordances;
- exact scope kinds, default policy, selector mapping, and resolution requirement;
- read/direct-command/confirmed-destructive/autonomous/workflow/internal effect;
- auth grants, idempotency, expected version, audit, compensation/recovery;
- supported output formats and deterministic default;
- presentation ID, column/item descriptors, detail levels, field visibility;
- ordering, cursor, page/default/hard caps, streaming/export behavior;
- coverage/freshness/redaction/retention and missing-state behavior;
- soft/hard response/token/time/memory budgets and truncation strategy;
- availability prerequisites and one safe remediation;
- documentation/example/completion/help links;
- for MCP: primitive kind, supported protocol revisions, JSON Schema dialect, `inputSchema`/`outputSchema`, title/icon/content-block/audience/priority policy, effect annotations, task support, required client/server capabilities, resource URI/template and mutability, prompt arguments, completion eligibility, subscription/list-generation owner, and namespaced `_meta` fields;
- V1 differential fixture and final deletion receipt.

### 5.3 Generator and drift rules

The generator consumes reviewed use-case definitions, domain/application schemas, presentation specs, and frozen V1 inventories. It emits clap metadata, MCP definitions, OpenAPI operation links, SDK/docs links, dashboard command metadata, shell completion, and conformance fixtures.

CI rejects:

- a command/tool/alias/hidden path absent from the inventory;
- a binding without a use case or a use case implemented in a transport;
- divergent required/default/enum/range/unit/scope/effect/error fields;
- a format advertised but not rendered, or rendered but not in the schema;
- a collection without cursor/order/cap metadata;
- raw `Value` accepted by a public renderer;
- a mutation without one exact effect mode;
- curation item approval/apply/reject/rollback bindings;
- missing or duplicate search-evaluation CLI/MCP/resource/HTTP/SDK/UI mapping, writable evaluation resource, any invented `retrieval.fixtures.*` binding instead of shared `experiments.fixtures.promote`, or any non-canonical search-evaluation alias;
- an active alias past cutoff;
- generated output drift or non-deterministic order.

### 5.4 First-class MCP adapter architecture

The V2 target remains a thin root adapter, as plan 19 requires, but “thin” means protocol-complete rather than hand-written and partial:

```text
src/mcp/
├── mod.rs                         # composition only
├── service.rs                     # official-SDK ServerHandler -> application port
├── session.rs                     # connection-local negotiated/auth/catalog state
├── lifecycle.rs                   # initialize/initialized/ready/draining/closed state machine
├── dispatch.rs                    # generated BindingId -> application execute
├── result.rs                      # typed view -> MCP content/structuredContent/resource links
├── error.rs                       # protocol error vs tagged tool-execution problem
├── roots.rs                       # capability-gated roots/list + list-changed invalidation
├── resources.rs                   # generated list/templates/read/subscribe/unsubscribe
├── prompts.rs                     # generated list/get only
├── completion.rs                  # prompt/resource-template argument completion
├── notifications.rs              # bounded per-session coalescing hub
├── operations.rs                 # progress, cancellation, task-augmented OperationRef bridge
├── client_requests.rs            # capability-gated sampling/elicitation only
├── auth.rs                        # principal/grants; no business authorization
├── generated/
│   ├── protocol.rs
│   ├── tools.rs
│   ├── resources.rs
│   └── prompts.rs
├── transports/
│   ├── stdio.rs
│   └── streamable_http.rs
└── conformance/
    ├── fixtures.rs
    └── host_profiles.rs
```

The official Rust SDK owns JSON-RPC types, framing, request/response multiplexing, standard method names, and stdio/Streamable HTTP protocol mechanics. TraceDecay pins the SDK/protocol versions and wraps them behind its root adapter; catalog generation supplies definitions and dispatch rows instead of per-handler macros or match arms. If a required stable-protocol feature is missing upstream, one bounded protocol adapter may be registered in plan 19's adapter ledger with an upstream issue, conformance fixtures, and a deletion release. No application behavior enters that adapter.

`McpSessionContext` is isolated per client connection and contains only negotiated protocol revision, client implementation/capabilities, authenticated principal/grants, captured roots, pinned `CatalogSnapshotRefV1`, `McpLogicalRegistrationId`, `McpSurfaceProfileId`, profile definition digest/effect/grant ceilings, config-registry digest, resolved default scope candidate, logging level, request/task cancellation registry, subscriptions, and list generations. The shared daemon engine, stores, caches, and application services are not connection state. A session state machine enforces:

1. `initialize` is the first request; only ping is tolerated around initialization as the stable specification permits.
2. The server compares the client revision to its generated supported set. It returns the same revision when supported or its current supported revision otherwise; an incompatible client disconnects/updates before application/store access. V2 does not retain the current `2024-11-05` live protocol.
3. The initialize result advertises only implemented standard capabilities and carries `CatalogSnapshotRefV1`, logical registration/profile IDs and profile digest, config-registry digest, and daemon generation in namespaced `_meta["io.tracedecay/catalog-v1"]`. Server composition validates the configured registration/profile pair before store access and never accepts a tool argument or client root as a profile selector.
4. Normal requests begin only after `notifications/initialized`. Server-to-client requests other than allowed ping/logging do not run earlier.
5. Every operation captures session/auth/catalog/scope state once. Concurrent requests may complete out of order by JSON-RPC ID, while one bounded writer task serializes valid protocol messages; no task writes directly to stdout.
6. Reader progress never waits for a long handler. Cancellation, client responses, root changes, pings, and drain signals remain processable while application work runs under bounded per-principal and per-capability concurrency.
7. Drain stops admission, returns a retryable typed problem for new calls, propagates cancellation only at cataloged safe boundaries, lets non-cancellable effects reach a durable receipt/reconciliation state, flushes audit state, and closes. Reconnect always performs a fresh initialize; no daemon proxy replays an old session as if it were current.

Bootstrap failure does not create a second “degraded MCP server.” Catalog and safe system/project/repair availability can initialize without opening a selected project store. The same generated definitions remain visible with typed availability; calls that require blocked identity/storage return one application problem. Recovery updates availability and emits a real list/resource change only if the authorized primitive set changed.

#### 5.4.1 Catalog refresh and dynamic lists

- `tools/list`, `resources/list`, `resources/templates/list`, and `prompts/list` are generated from the pinned profile, access-filtered, and protocol-paginated. Their opaque cursor binds principal, protocol revision, logical registration, profile digest, catalog/list generation, and ordering, so pages never mix snapshots or profiles.
- Missing host prerequisites remain visible as safe unavailable tool metadata when visibility is authorized; a capability is hidden entirely when even its existence is unauthorized. Counts, completion, list-change events, and errors cannot reveal hidden rows.
- A catalog, availability, or grant change inside the fixed profile increments the affected primitive generation. A per-session notification hub emits at most one coalesced `notifications/tools/list_changed`, `notifications/resources/list_changed`, or `notifications/prompts/list_changed` for that generation, and only when the matching capability was advertised. List change cannot select another profile, cross a registration trust boundary, or disclose a prompt-specific tool; widening requires installer/config change plus reconnect.
- Once a session is marked refresh-required, a call that depends on a replaced binding fails with `capability_replaced`/`client_update_required`; it never dispatches by a stale name to new semantics. A complete list refresh atomically pins the new generation. A protocol or binary-major change requires reinitialize/reconnect instead.
- `resources/subscribe` is offered only for mutable typed resources with an application/projector change feed. `notifications/resources/updated` carries the authorized URI, not payload. Reads and every delivery reauthorize; revocation removes the subscription. Slow consumers receive one coalesced update/gap instruction and reread rather than accumulating unbounded deltas.
- `logging/setLevel` stores a connection-local syslog threshold. Protocol log notifications contain safe structured diagnostics/correlation IDs, never result payloads, secrets, raw prompts, or progress that belongs in `notifications/progress`.
- Every protocol log notification includes the immutable producer `TraceDecayBuildRefV1`; a daemon/proxy/collector adds its own collector build without replacing the producer. Generated log/diagnostic use cases accept the one `TraceDecayVersionSelectorV1`, exposed consistently as exact/range/include/exclude/current-runtime-set/compatible-protocol/legacy-unknown controls in CLI, MCP, HTTP, SDK, and dashboard. Output echoes the immutable runtime build set or compatibility-manifest selection plus searched/returned/excluded/unknown counts. Human stderr lines carry the producer version; JSON/NDJSON uses the same typed field, never a renderer-only prefix.

#### 5.4.2 Roots, scope, and client capability use

If a client advertises roots, TraceDecay requests `roots/list` after initialization and reacts to `notifications/roots/list_changed`. Roots are untrusted scope candidates only: they are canonicalized through plan 16, authorized, and compared to registered projects/worktrees. They never override an explicit `ScopeSelectorV2`, choose the first matching checkout, or silently change the active graph. A changed root invalidates the candidate and requires a new resolution; it does not mutate an in-flight request.

Sampling and elicitation are optional client capabilities, not generic fallbacks:

- client sampling may be requested only by an explicit foreground catalog use case or a playground/evaluation operation whose policy, sanitized prompt manifest, model/tool budget, approval, result visibility, and audit receipt are visible. Plan 22's daemon/Spark scout and autonomous curation use their owned model gateway; they never silently spend the connected host's model through MCP sampling.
- sampling with tools requires the client's negotiated `sampling.tools` capability and a generated bounded tool subset. The client remains free to choose/deny the model and response; sampled content re-enters as untrusted input and passes plan 18.
- form elicitation cannot request a password, API key, access token, confirmation secret, or payment credential. URL elicitation is the only MCP path for sensitive/OAuth interaction, uses validated HTTP(S) URLs, binds state to the principal/session, and never shells out.
- elicitation may resume an application operation already in `WaitingForInput`; it does not invent item approval for autonomous curation. A client without the capability receives `interaction_required` and an operation-specific safe next action.

#### 5.4.3 Progress, cancellation, and MCP protocol tasks

For an ordinary request containing `_meta.progressToken`, the adapter maps cataloged application progress into monotonic `notifications/progress`; tokens are opaque, session-local, and never persisted as domain identity. `notifications/cancelled` cancels only the referenced in-flight request in the same direction/session and produces no response after a successful cancellation. Completion/cancellation races are deterministic and audited.

Long-running use cases may declare MCP `execution.taskSupport = optional|required` only when the application returns a durable `OperationRef` with bounded TTL, status, result, cancellation boundary, principal/scope binding, and rate limits. `McpTaskId` is an opaque protocol projection of that operation. `tasks/list|get|result|cancel` never query or mutate plan 24 task/work-item tables by ID coincidence; a plan-24 work item is visible through its own generated TraceDecay resource/tool bindings. Ordinary request cancellation uses `notifications/cancelled`; task-augmented execution uses `tasks/cancel`.

#### 5.4.4 Transports and authorization

Stdio is the mandatory local host transport. It authenticates the installation/launch context, accepts only MCP JSON on stdout/stdin, sends diagnostics only to stderr or negotiated logging, minimizes inherited environment, and applies catalog grants exactly like every other surface.

An optional `/mcp` endpoint implements current Streamable HTTP directly through the official SDK; it is not `/api/v2`, an OpenAPI bridge, or the removed HTTP+SSE transport. It validates `Origin`/Host, binds loopback by default, accepts the negotiated `MCP-Protocol-Version` header, uses cryptographically strong `MCP-Session-Id` where stateful sessions are enabled, supports safe SSE resumption without cross-stream replay, and applies bounded connection/session/request queues. Loopback authentication consumes plan 17's scoped token registry. Any future non-loopback deployment must implement the official MCP OAuth protected-resource/resource-indicator contract, TLS, audience validation, incremental scopes, and a separate reviewed threat model. TraceDecay never accepts tokens in URLs/tool arguments, passes the client token through to another service, or uses MCP annotations as authorization.

### 5.5 One adapter, optional exposure profiles, and component sets

TraceDecay ships one MCP implementation. The generated host installer may write up to three logical registration entries, but every entry starts the same thin `tracedecay` integration binary, connects to the same private `tracedecayd` application authority, and loads the same catalog snapshot. Registration names exist only to establish visible-surface and grant trust boundaries:

| Registration | Allowed profiles | Intended surface | Initial hard ceiling |
|---|---|---|---|
| `tracedecay-context` | `agent-core`, `developer`, `research` | read-only capability/scope/project/search/anchor/operation core plus reviewed code/Git or session/knowledge research reads | 12/8k, 32/24k, or 24/18k tools/definition tokens respectively |
| `tracedecay-work` | `task-worker`, `orchestrator` | addressed task/packet/attempt lifecycle or authorized initiative/plan/task orchestration and edit bundles | 24/16k or 32/24k respectively |
| `tracedecay-operator` | `operator`, `admin-lab` | configuration/system/automation/migration administration or explicitly budgeted administration labs | 24/18k or 32/24k respectively; explicit opt-in only |

The generated install state is a nonempty `HostInstallSetV1`: optional `CoreSkillsCli` plus at most one `McpFacade { registration, profile }` component for each logical registration. Empty means no desired deployment and is represented by absent/retired desired state through uninstall, not an empty set. Core is complete and portable on shell-capable hosts: every skill declares `UseCaseId` requirements, resolves the cheapest legal installed binding, and can issue the generated CLI or documented HTTP recipe when MCP is absent. A headless facade-only set is explicit and retains the same use cases; it is not the preferred shell-capable install. Core plus zero/one/many companions compose without copying semantics. Installation never silently adds operator, and uninstall/update removes or rewrites only manifest-owned component entries after foreign-ownership checks.

Plan 08 generates `mcp-surface-profiles.json`. Every row is a canonical, sorted, explicit `BindingId` set with registration, audience, required host features, allowed execution modes, required grants and grant ceiling, tools-only fallback bindings, maximum tool count, maximum definition tokens plus the versioned estimator, and definition digest. Prefix/category/domain globs are forbidden: a newly cataloged tool is not exposed until a reviewed profile diff names it. The session computes `profile bindings ∩ negotiated primitive support ∩ authenticated grants ∩ current authorization`; denial wins and a profile never grants authority.

Deferred tool search is an optional host optimization over that already fixed set. Eager hosts receive the complete profile definition list and must pass the same tool-selection, token, latency, and task-completion corpus. A host that lacks resources/prompts/completion receives only cataloged tools-only fallbacks of the same `UseCaseId`; it does not receive a generic `invoke(binding,args)` escape hatch. Resource and prompt assistance can improve ergonomics but cannot be required for task completion unless the host manifest says the feature is mandatory.

Multiple registrations do not partition data or create independent business services. They isolate connection state, visible bindings, and grant ceilings only. Cross-domain graph/query results still come from the one application/query platform, and a saved/retrieval anchor is equally resolvable when the receiving principal/profile is authorized. Profile analytics record installation mode, registration/profile digest, eager/deferred discovery, candidate/selected/missed binding, definition tokens, and unavailable fallback without recording tool arguments or results.

## 6. Canonical ownership and dependency flow

```text
domain schemas + application use cases + reviewed presentation specs
                              │
                              ▼
                  tool catalog / generators
                              │
             ┌────────────────┼──────────────────┐
             ▼                ▼                  ▼
       CLI bindings      MCP bindings       HTTP/SDK/docs/UI
             │                │                  │
             └──────────── application ──────────┘
                              │
                              ▼
                ApplicationResponse<TypedView>
                              │
          ┌───────────────────┼────────────────────┐
          ▼                   ▼                    ▼
 canonical JSON       presentation document   stream/export rows
                          │          │             │
                          ▼          ▼             ▼
                       Markdown   terminal       NDJSON/SSE
```

| Concern | Sole owner | Forbidden duplicate |
|---|---|---|
| Semantic request/result/effect/error | domain + application | CLI args or MCP handlers redefining behavior |
| Capability/use-case/binding metadata | tool catalog | handler match lists and plugin schema forks |
| Scope resolution | application scope resolver | CWD/route/daemon/handler first-match logic |
| Machine JSON | sealed `ApplicationResponse<T>` serializer | JSON assembled in renderers or parsed from Markdown |
| Human presentation | Root `v2::presentation` module | native-command `println!` layouts and raw-Value Markdown |
| HTTP/NDJSON/SSE envelopes | API plan 10 | CLI/MCP transport inventing stream protocols |
| MCP lifecycle/framing/session/standard methods | official SDK + root MCP adapter | daemon proxy, degraded server, or tool handler reimplementing protocol state |
| MCP primitive definitions/capability requirements | tool-catalog generated manifests | server-local resource/tool/prompt arrays or notification allowlists |
| Configuration | plan-20 registry/application | CLI-only flag or dashboard-only setting |
| Privacy eligibility/redaction | plan 18 | output-specific string scrubbing |
| stdout/stderr/exit mapping | generated CLI adapter | command-module process exits |
| MCP result/problem mapping | generated MCP adapter | handler-local status prose |

### 6.1 Root `v2::presentation` scope

Add the small pure root-private module below. Plan 19 locks this deployment choice: CLI and MCP share it inside root, while documentation snapshots and conformance runners test the same facade. A separately published crate would add a package/release boundary without a second production consumer.

```text
src/v2/presentation/
├── mod.rs
├── document.rs
├── spec.rs
├── budget.rs
├── markdown.rs
├── terminal.rs
├── table.rs
├── labels.rs
├── problems.rs
├── progress.rs
└── escape.rs
tests/
├── presentation_v2.rs             # integration-test harness
└── presentation_v2/
    ├── golden.rs
    ├── parity.rs
    ├── width.rs
    ├── injection.rs
    └── secret_canary.rs
```

Allowed imports: domain safe value types, application public view types, generated presentation descriptors, Unicode width helpers, and pure serialization/test libraries.

Forbidden imports: stores, queries, policy execution, hooks, providers, Axum, rmcp, clap parsing, SQL, Git/network clients, filesystem/process access, environment/time reads, and `serde_json::Value` in public renderer APIs.

Plan 19's allowed-edge manifest permits root `v2::presentation -> tracedecay-domain`, `tracedecay-application` public view contracts, and generated catalog presentation descriptors only. The module has no edge to store/capture/projectors/query/policy/hooks/API implementations or ambient root services. CLI/MCP adapters depend on this one presenter; no second root-local presenter is permitted after it lands.

Migration starts by extracting the useful pure pieces already present in `src/mcp/tools/render.rs` (`OutputFormat`, Markdown escaping/document helpers, truncation metadata) behind the new typed facade, then deletes or declarativizes the 36 inventoried handler-local `render_*` functions. Existing behavior is a differential fixture, not permission to wrap both paths indefinitely.

## 7. Typed view and presentation model

### 7.1 Semantic views

Application use cases return domain-specific transport-eligible structs, not arbitrary maps:

```rust
pub struct SearchResultsViewV2 {
    pub query: SafeQuerySummary,
    pub items: CursorPage<SearchResultViewV2>,
    pub facets: SearchFacetSummaryV1,
    pub ranking: RankingReceiptV1,
}

pub struct ProjectRegistryViewV2 {
    pub projects: CursorPage<ProjectListItemViewV2>,
    pub project_tree: Vec<ProjectTreeNodeViewV2>,
    pub summary: ProjectRegistrySummaryV2,
    pub active_project_id: Option<ProjectId>,
    pub registry_state: RegistryCoverageV2,
}

pub struct CommandReceiptViewV2 {
    pub operation: OperationRef,
    pub outcome: CommandOutcomeV2,
    pub effects: Vec<EffectReceiptV2>,
    pub recovery: RecoveryDispositionV2,
}

pub struct WorktreeLifecycleViewV2 {
    pub worktree: WorktreeSummaryV2,
    pub external_creator: ExternalWorktreeCreatorProvenanceV1,
    pub associations: CursorPage<TaskWorktreeAssociationViewV2>,
    pub references: WorktreeReferenceSummaryV1,
    pub eligibility: WorktreeCleanupEligibilityV1,
    pub current_intent: Option<WorktreeCleanupIntentViewV2>,
}
```

Every content-bearing field is a plan-18 eligible wrapper or an explicit redacted/denied/unknown variant. `ApplicationResponse<T>` carries scope, snapshot, coverage, freshness, redactions, retention, limits, and warnings once.

Worktree association and cleanup presentations never collapse candidate confidence, ambiguity, creator/source provenance, cleanup-grant authority, blockers, underlying references, preview digest/expiry, operation state, or receipt/failure/reconciliation into prose-only status. Ticket, CLI, MCP, HTTP, and SDK consume the same view; plan 11 may choose a compact card but cannot infer eligibility from an archived label or merged badge.

### 7.2 Human document IR

Presentation converts a typed view into a bounded semantic document:

```rust
pub struct HumanDocument {
    pub title: CatalogSafeText,
    pub summary: Vec<DocumentBlock>,
    pub body: Vec<DocumentBlock>,
    pub coverage: CoverageBlock,
    pub next_actions: Vec<NextActionBlock>,
}

pub enum DocumentBlock {
    Heading(HeadingBlock),
    FieldList(FieldListBlock),
    ItemList(ItemListBlock),
    Table(TableBlock),
    Code(CodeBlock),
    Notice(NoticeBlock),
    Progress(ProgressBlock),
    Empty(EmptyStateBlock),
    Truncation(TruncationBlock),
}
```

The IR contains typed cell values, links/anchors, severity, and wrapping hints rather than embedded Markdown/ANSI. Markdown and terminal renderers escape from this IR. JSON never serializes this IR; it serializes the original semantic view.

### 7.3 Generated presentation traits

```rust
pub trait PresentHuman: TransportEligibleView {
    const PRESENTATION_ID: PresentationId;
    fn to_document(&self, context: &PresentationContext) -> HumanDocument;
}

pub struct PresentationContext {
    pub binding: BindingId,
    pub locale: LocaleId,
    pub width: Option<u16>,
    pub color: ColorPolicy,
    pub detail: DetailLevel,
    pub budget: PresentationBudget,
}
```

Implementations are domain-clustered and generated/checked against field descriptors. There is no catch-all raw JSON renderer. Unknown extension views must supply a versioned presentation plugin or fall back to canonical JSON only with an explicit unavailable-human-format reason.

## 8. Output format contract

### 8.1 Format matrix

| Surface | Default | Explicit formats | Rules |
|---|---|---|---|
| CLI query/status | `human` | `human`, `table`, `markdown`, `json` | Default never changes with TTY. TTY affects color/width only. `--json` is a generated alias for `--format json`, not another mode. |
| CLI stream/watch | `human` | `human`, `ndjson` | NDJSON is one complete typed event per line with initial metadata and terminal coverage/summary. `--jsonl` aliases `--format ndjson` until cutoff. |
| CLI export | operation-specific | `ndjson`, `json`, `csv`, `parquet` only where cataloged | Export formats are schema-versioned data products, not console rendering. CSV remains only for flat declared schemas. |
| MCP | `markdown` | `markdown`, `json` | JSON is canonical semantic data; Markdown is compact human/agent presentation. No table/ANSI. |
| HTTP/SDK | `json` | JSON plus cataloged NDJSON/export media | Follows plans 10/17 content negotiation. |
| Subscription | SSE | SSE only | Snapshot/delta/progress/gap protocol from plans 10/17; not line-oriented renderer output. |
| Dashboard | typed client view | UI components/export actions | No Markdown scraping or CLI execution. |

### 8.2 Canonical CLI switches

Generated public commands share only applicable global switches:

```text
--format human|table|markdown|json|ndjson
--scope <typed-selector-json-or-reference>
--profile <id-or-safe-alias>
--project <id-or-safe-alias>
--repository <id-or-safe-alias>
--checkout <id-or-safe-alias>
--worktree <path-or-id>
--ref <git-ref>
--snapshot <snapshot-id>
--consistency authoritative|bounded-stale|offline-cache|as-of-watermark
--freeze-pagination
--limit <n>
--cursor <opaque-cursor>
--fields <generated-field-set>
--detail compact|normal|full
--no-color
--quiet
--verbose
```

Ergonomic flags are lossless builders for `ScopeSelectorV2`. They are mutually exclusive with `--scope` when they overlap. Generated help shows the canonical selector before invocation. Mutations require an explicit durable target; queries use a default only when the catalog declares it, such as profile-wide `AllAuthorized` for Brain.

No global flag shadows a semantic input field. Catalog-backed `tracedecay help schema <binding>` exposes the input contract, while each generated command validates its own typed input before dispatch; there is no public generic `invoke(binding,args)` bridge. Semantic `dry_run` is removed in favor of the use case's declared execution mode. Debugging the raw transport envelope uses a hidden test-only developer command, never public `--json`.

### 8.3 Canonical MCP result

Every tool publishes generated JSON Schema 2020-12 `inputSchema` and an object-root `outputSchema`. The output is a tagged union of `{ outcome: "ok", response: ApplicationResponse<T> }` and `{ outcome: "error", problem: SurfaceProblemV2 }`, so success and tool-execution error `structuredContent` both validate. MCP bindings accept a generated `format` enum only where human rendering is supported. The adapter returns:

- `structuredContent`: the canonical tagged typed outcome for every current-protocol tool result;
- `content`: compact Markdown from the same typed view in default mode, including stable IDs, coverage, active target, limits, and legal next action;
- explicit `format=json`: the same canonical outcome serialized once into a text content block, while `structuredContent` remains the object; no JSON string is nested inside another semantic JSON field;
- an MCP `resource_link` plus compact summary when a durable retrieval anchor/resource is the safe bounded representation; the link never points at a project-local response-handle cache;
- namespaced `_meta` containing safe binding/catalog/protocol/request IDs and presentation receipt only, never semantic fields duplicated from `ApplicationResponse` or data a client must preserve to understand the result;
- `isError=false` for the success variant and `isError=true` for a known tool's application/validation/authorization/business failure.

Unknown tool names, malformed JSON-RPC, invalid MCP request structure, unsupported methods, and protocol-state violations are JSON-RPC protocol errors. A well-shaped call to a known tool returns the typed tool-execution error so the model can self-correct. Raw internal failures become a safe correlation-bearing tool error unless the protocol connection itself is unusable. The default Markdown plus structured-object choice is intentional: V2 records a reviewed SHOULD-level deviation from the specification's backwards-compatibility serialized-JSON text echo in default mode, avoids doubling every human result, provides the echo in explicit JSON mode, and claims no pre-structured-content protocol compatibility. Conformance decodes Markdown-default, explicit-JSON, resource-link, success, and error paths back to the same canonical fixtures.

### 8.4 Determinism

- maps render in schema-defined order, never hash insertion order;
- rows sort by the use case's declared primary keys and stable ID tie-breaker;
- time uses captured request context and canonical UTC machine values; human relative time includes exact time at normal/full detail;
- sizes, durations, scores, money, tokens, paths, enums, and counts have canonical units;
- color is decoration only; stripping ANSI produces the no-color bytes except for intentional width padding;
- terminal width changes wrapping/column selection, never row membership or meaning;
- locale affects approved human labels only; machine fields/enums/decimal syntax remain stable.

## 9. CLI information architecture and navigation

### 9.1 One command tree

The generated CLI groups capabilities by product domain rather than implementation history:

```text
tracedecay brain       profile-wide search, graph, timeline, inspect
tracedecay code        code graph, health, diagnostics, edit
tracedecay git         refs, changes, delivery correlation
tracedecay sessions    sessions, messages, Turns, agents, workflows, goals
tracedecay memory      knowledge status, query, autonomous curation outcomes
tracedecay automation  schedules, dirty scopes, admissions, runs, outcomes, authority, health
tracedecay scout       status, addressed suggestions, explanation, evaluation, feedback, runtime control
tracedecay config      complete plan-20 settings tree
tracedecay integration generated host package/component/registration inventory and lifecycle
tracedecay project     registry, repositories, worktrees, enrollment, sync
tracedecay system      status, doctor, daemon, update, migration, accounting
tracedecay experiment  fork/create/run/list/get/cancel/resume/retry/minimize/export replay
tracedecay task-graph  status, doctor, events, and edit-bundle workflow
tracedecay tool        schema-exact named CLI fallback over current generated bindings
tracedecay help        catalog-backed discovery
```

The `system` group also exposes plan 17's scoped, TTL-bound, revocable local API-token surface with exact effect classes: `api token list` binds the elevated read `auth.tokens.list`, while `api token create` and `api token revoke` bind audited commands. HTTP parity is `GET /api/v2/auth/tokens`, `POST /api/v2/commands/auth/tokens:create`, and `POST /api/v2/commands/auth/tokens:revoke`; MCP/CLI bindings preserve those semantics without copying HTTP route syntax. Plan 10's per-launch bearer remains only the bootstrap credential permitted to invoke the initial `auth.tokens.create` command.

Database administration is daemon-only and generated under `tracedecay system storage authority|isolation|integrity|snapshots|checkpoint`. Reads expose safe authority/isolation/watermark/receipt state; operator mutations invoke the plan-09 workflows and never accept or print a path, SQLite URI, SQL, raw backup, WAL/page bytes, key, or file descriptor. MCP ordinary/context/work profiles expose no database administration or physical schema; the operator profile may expose only these typed daemon workflows.

Plan 28 freezes this transport-agnostic subtree:

```text
tracedecay system brain status
tracedecay system brain join|leave
tracedecay system brain nodes list|show|rotate|revoke
tracedecay system brain placements list|plan|apply|verify
tracedecay system brain sync status|run|pause|resume|repair
tracedecay system brain replicas list|seed|verify|retire
tracedecay system brain repositories candidates|adopt|split
tracedecay system brain backup status|verify
tracedecay system brain failover plan|promote|verify
```

Selectors are opaque Brain/node/shard/repository refs or safe authority URLs; no flag accepts a database/WAL path, database credential, raw node key, or generic SQL. `join` works with ordinary HTTPS/mTLS. A Tailscale/MagicDNS example is optional documentation, never a command mode or dependency. Mutations require generated operator grants, expected versions, authority/placement epochs, idempotency, and operation receipts.

The default `tracedecay-context` MCP component exposes only compact `brain_status`/coverage and actionable retrieval IDs. Node/placement/sync-repair/revoke/replica/failover mutations exist only in the explicitly installed operator component and remain least-privilege. MCP never exposes sync chunks, certificate/key material, credentials, raw store locations, or transport internals; skills plus CLI remain complete without MCP.

`experiment fork` accepts any catalog-compatible message/Turn/session/agent/tool/hint/fact/task/policy/code/Git/retrieval anchor and returns the same typed draft/source backlink as the dashboard's Fork to Playground. `create` consumes the tagged `LabKindV1` schema; `run` returns the shared `OperationRef`; experiment/run/cell/stage/comparison/comparison-cell/reduction list/get use the one generated top-level filtered operation and addressable read-only MCP resources where supported; cancel/resume/retry/minimize are generated tools/CLI commands over the one operation lifecycle. Every cell renders its variant/evaluator/corpus-case/repetition/sweep coordinate and anchor. MCP protocol task augmentation may wrap a long experiment run, but `McpTaskId`, `OperationId`, plan-24 `WorkItemId`, and `ExperimentRunId` remain distinct. No per-lab tool-name explosion or lab-owned progress/cancel schema is generated.

Optional semantic code search adds no operation or alias. `tracedecay brain search` and `tracedecay code search-symbols` remain bindings of `search.universal` and `code.search_symbols`; their generated request/profile flags select strict versus lexical fallback, native semantic/native rerank enablement, and rerank top-N (default 25, hard cap 25). Human and JSON output expose the same FastEmbed embedding plus optional BGE rerank stage waterfall, desired/activated/effective/observed state, exact artifact/model/generation, rebuild/coverage, rank/rerank delta, latency/RSS/cache/vector/index coverage, provenance, and typed error/fallback. Missing optional stages preserve the byte-stable lexical order unless strict mode requests an error.

The generated `code.redundancy` successor of current health `redundancy`, plus changed-file simplification, accepts the same cataloged optional semantic profile; neither gains a FastEmbed-specific name or transport-local threshold. Their shared typed view returns canonical entity pairs, exact/structural/semantic-analogue class, component scores and matched fields, scope/snapshot/profile/vector-generation anchors, baseline-versus-semantic contribution, exclusions, truncation, and coverage. Semantic-only pairs are explicitly advisory. Disabled or failed semantics returns byte-identical baseline pair rows/order/explanations plus typed omitted coverage, and CLI, MCP, HTTP, SDK, dashboard, export, and lab fixtures assert that parity. Current graph `similar` maps separately to bounded name/signature similarity and cannot silently acquire duplicate-code semantics.

The one operator CLI lifecycle is `tracedecay system representations artifacts list|show|status|install|import|activate|deactivate|evict|verify` and `tracedecay system representations generations list|rebuild`, generated directly from `representations.artifacts.*` and `representations.generations.*`. Install/import accepts exact manifest/model and explicit download/egress consent; no command accepts a cache/database path or starts a model in the client. Ordinary context/developer MCP profiles expose the existing search reads only when already reviewed for those profiles. Representation mutations remain operator-profile tools; list/get/status may be reviewed read-only resources/tools. MCP never gains `fastembed`, `semantic-code`, `spark-rerank`, or generic model dispatcher names.

A separately registered optional Codex Spark/app-server-style rerank profile is configured and surfaced inside the same search request/result. It is off by default, supplies no embeddings, and never replaces the promoted FastEmbed embedding or native BGE reranker. CLI/MCP parity preserves discovered capability, credential reference (never value), privacy/egress decision, exact model, cost/token/deadline/top-N budgets, requested/actual route receipts, and typed unavailable/timeout fallback with unchanged pre-rerank order. Search Quality uses the existing experiment CLI/MCP family for replay/ablation; plan 22 active hinting/scout may consume the same capability registry, not a new CLI/MCP binding family.

Host integration freezes this exact public CLI tree:

```text
tracedecay integration list
tracedecay integration show <installation-ref>
tracedecay integration diff <target-or-installation-ref>
tracedecay integration status <target-or-installation-ref>
tracedecay integration install <target-ref>
tracedecay integration update <installation-ref>
tracedecay integration repair <installation-ref>
tracedecay integration uninstall <installation-ref>
tracedecay integration verify <installation-ref>
```

`list|show|diff|status` bind `integrations.list|get|diff|status` and are admin-scoped reads; status uses persisted observation and never probes. The five operation bindings map to `integrations.install|update|repair|uninstall|verify`, require the admin host-integration grant plus expected desired/observed/manifest versions and idempotency, return the shared `OperationRef`, and use ordinary operation status/progress/recovery rendering. Install/update/repair/uninstall may change owned host state; `verify` performs only the fresh probe. All selectors are opaque catalog refs; no flag accepts a host path, raw configuration body, command line, environment, credential value, arbitrary manifest, or generic action name.

`install` and `update` accept generated selection flags for the target's desired `HostInstallSetV1`: signed base/companion package IDs, install scope, skills/roles/hooks component enablement, and zero/one/many context/work/operator logical registrations with one allowed profile each. The CLI sends the same typed request as HTTP/SDK/MCP; it never writes host state itself. `diff` renders the canonical capability-difference matrix and `show|status` retain package/component versions and digests, ownership/trust/stale-cache, MCP exposure, effective/drift/restart, active operation, and legal next actions. Markdown/terminal and JSON come from the same view.

The deprecated aliases `install`, `claude-install`, `reinstall`, `update-plugin`, `update-plugins`, `uninstall`, and `claude-uninstall` live only in the migration inventory with replacement/cutoff metadata. Before cutoff they lower to the corresponding canonical use case and emit the generated typed deprecation notice; at cutoff they return `capability_replaced` without performing work. They are never generated as MCP tools, HTTP routes, SDK methods, or current skills.

MCP remains optional. Core skills and generated CLI recipes can execute every integration workflow on a shell-capable host; the official admin HTTP/SDK is the headless fallback. If MCP exposure is explicitly installed, only the `tracedecay-operator` registration receives the reviewed admin integration bindings and their same schemas/operations—there is no installer god tool or second implementation. The three logical registrations, component-set configuration, CLI, API, SDK, dashboard, and optional MCP all call plan 09's one application feature and root `HostDeploymentPort`.

Live attempt steering uses this exact generated CLI tree:

```text
tracedecay task-graph comments list --work-item <id>
tracedecay task-graph comments add --work-item <id> (--text <bounded>|--stdin)
tracedecay task-graph comments revise|tombstone --comment <annotation-id> --expected-revision <n> ...
tracedecay task-graph steering list --attempt <id>
tracedecay task-graph steering show <directive-id>
tracedecay task-graph steering submit --attempt <id> --requirement advisory|required --kind <closed-kind> (--text <bounded>|--stdin) --expected-lease <id> --authority-epoch <n> --fence-epoch <n> --expected-packet <ref> --expected-graph-revision <n> --expected-steering-version <n> --priority <level> --expires <utc> --idempotency-key <key>
tracedecay task-graph steering promote --comment <annotation-id>@<revision> --body-digest <digest> --attempt <id> <same fenced steering fields>
tracedecay task-graph steering acknowledge <directive-id> --attempt <id> --steering-sequence <n> --delivery-receipt <id>
tracedecay task-graph steering resolve <directive-id> --disposition applied|rejected --evidence <anchor>
tracedecay task-graph steering supersede <directive-id> --by <higher-sequence-directive-id>
tracedecay task-graph steering cancel <directive-id> --expected-steering-version <n> --reason <closed-code>
```

Comments are the shared annotation use cases rendered through a task-specific facade; there is no task-comment store or lifecycle. `promote` is the explicit source-comment form of the same `task_steering.submit` use case as `submit` and pins the exact `TaskCommentRevisionRefV1`. Neither comment creation nor `comments list` delivers content to an agent. Steering output always leads with work item/attempt, lease plus authority/fence epoch, monotonic sequence, expected packet/graph revision, requirement, expiry, actual boundary disposition, acknowledgement/resolution, and whether an unresolved required directive currently fences completion/integration. It also renders effective versus absolute payload/batch/Turn/rate/cooldown limits and the pinned digest when relevant; stdin/text is measured before submission, never truncated. `accepted` is never rendered as `delivered`; advisory rows are visibly non-blocking.

The MCP surface stays compact: one read-only paginated resource template `tracedecay://v2/execution-attempts/{attempt_id}/steering` plus exactly three tools, `task_steering.submit`, `task_steering.acknowledge`, and `task_steering.disposition`. `submit` accepts either a bounded direct payload or an exact comment-revision ref, never both. `disposition` has a closed tagged union: `Resolve { Applied|Rejected, evidence }`, `Supersede { higher_sequence_directive }`, or `Cancel { expected_version, reason }`. It is a generated MCP facade only, not a semantic use case: Plan 08 maps each tag explicitly to the separate application `resolve`, `supersede`, or `cancel` command and preserves that command's schema, grant, idempotency, receipt, and errors. The `task-worker` profile receives its addressed resource plus acknowledge and the Resolve case within its attempt grant; `tracedecay-work`/`orchestrator` may receive submit and controller-authorized Supersede/Cancel cases. Context/developer profiles receive at most the read resource. There is no stream-to-model, generic comment tool, arbitrary prompt, `send-now`, interrupt, or adapter selector. Tools return compact Markdown plus canonical structured content; long evidence is retrieval-anchor linked.

All clients subscribe through the canonical `task_graph.events` subscription and resume by event ID. CLI `--watch` renders one ordered NDJSON/terminal stream of directive, delivery, acknowledgement, and resolution deltas; gap/resync reloads the authoritative attempt steering page. Required and delivery-unknown events cannot coalesce away. The client never polls an adapter, retries unknown model-visible delivery, or interrupts an in-flight tool. Host-safe delivery and one-shot Stop continuation remain daemon/hook behavior; the CLI/MCP only submit and disposition canonical commands.

Complex task/plan graph editing uses this exact CLI tree:

```text
tracedecay task-graph edit start <selection-ref> [--managed-dir|--archive <path>]
tracedecay task-graph edit get <workspace-id> [--managed-dir|--archive <path>]
tracedecay task-graph edit validate <workspace-id> (--bundle-dir <path>|--archive <path>)
tracedecay task-graph edit diff <workspace-id> --candidate-generation <n> --candidate-digest <digest>
tracedecay task-graph edit rebase <workspace-id> --candidate-generation <n> --candidate-digest <digest>
tracedecay task-graph edit submit <workspace-id> --candidate-generation <n> --candidate-digest <digest> --expected-plan-version <n>
tracedecay task-graph edit clean <workspace-id>
```

`start` invokes `task_graph.edit_bundles.export`, freezes the authorized plan/task graph version and scope, persists a protected expiring staging bundle through the shared operation/structured-staging kernel, and materializes either a sharded directory or the deterministic contained tar. The managed default is a TraceDecay-owned `0700` directory containing `0600` `manifest.md`, plan/work-item shards, reference stubs, and lock files; a CLI-local cleanup sidecar records workspace ID, owned root, complete member/device/inode manifest, and expiry. `--archive` creates a caller-owned tar that TraceDecay never deletes. `get` rematerializes the exact current candidate in either form after authorization. The server stores protected staged content, candidate reference, validation/diff receipts, and expiry; it never stores or returns the client-local path through MCP/HTTP.

`manifest.md` carries exact `TaskGraphEditManifestV1`; every editable CommonMark shard begins with one closed, versioned YAML 1.2-subset frontmatter mapping. Stable IDs/local keys, explicit retain/replace/retire intent, and the plan-24 file grammar own structured plan/item/dependency/gate/acceptance/route state; Markdown bodies own narrative only. Duplicate keys, aliases/custom tags, multiple YAML documents, unknown fields, excessive nesting, invalid UTF-8/control text, implicit deletion, and editable lease/fence/attempt/readiness/status/outcome/audit fields are rejected. Omission preserves an existing item. Frontmatter and bodies pass plan 18 before staging or diagnostics.

`validate` is the only CLI stage that uploads edited bytes. It deterministically packs `--bundle-dir` or checks the supplied `--archive`, then performs parse, schema, identity/version, reference, graph, policy, scope/authorization, active-lease, route/budget, and privacy validation. Success returns application-minted `TaskGraphEditCandidateRefV1 { workspace_id, generation, digest }`; failure returns ordered exact `TaskGraphEditDiagnosticV1` values and enclosing coverage. Markdown is the human/MCP default; explicit JSON preserves the identical typed values. `diff` accepts only that pinned candidate reference and returns the canonical semantic delta without re-reading or re-uploading a local path.

`rebase` consumes the pinned candidate reference, creates a successor candidate over the newest plan version, and reports exact `TaskGraphEditConflictV1`; it never reads another local path, writes conflict markers, or guesses a merge. `submit` consumes the same pinned reference, re-runs every validation against its digest/base/catalog/config/scope, and commits one bounded owner-shard plan/work-item/edge transaction with expected versions, idempotency, per-item dispositions, event/audit records, and one receipt. It cannot steal an active lease, set derived status, execute a task, or perform an external effect. `clean` invokes idempotent `task_graph.edit_bundles.delete`, makes protected payload GC-eligible, and recursively deletes a local managed directory only after root containment, owner, complete member/device/inode, non-link, and recorded-manifest checks. Submit success also performs that safe managed cleanup; validation/conflict failure retains the directory. Expired server workspaces are consumed from an indexed due queue rather than a repeated all-workspace scan; CLI startup processes only due managed-directory sidecars under its owned staging root and refuses changed or foreign entries.

MCP exposes generated tools for export/get/validate/diff/rebase/submit/delete and the read-only template `tracedecay://v2/task-graph/edit-bundles/{workspace_id}`. Small authorized documents may be embedded; a bundle above the binding's plan-08 `SurfaceBudgetV1.max_inline_bytes` returns a compact Markdown summary plus `resource_link`, current `TaskGraphEditCandidateRefV1`, size/expiry, and the exact CLI/API continuation. Resources are never writable. Only `validate` accepts candidate bytes; diff/rebase/submit require the pinned candidate reference. Only the `tracedecay-work` `orchestrator` profile contains the mutating export/rebase/submit/delete bindings; tools-only hosts get cataloged same-use-case read fallbacks, not a generic dispatcher.

Task-associated worktree lifecycle uses this exact generated CLI tree:

```text
tracedecay task-graph worktrees list --work-item <id>
tracedecay project worktree list|show [<worktree-ref>]
tracedecay project worktree discover [--repo <repository-ref>]
tracedecay project worktree association list [--work-item <id>|--worktree <id>]
tracedecay project worktree association diagnose --work-item <id>|--worktree <id>
tracedecay project worktree association associate --work-item <id> --worktree <id>
tracedecay project worktree association confirm|reject --association <id> --expected-version <n>
tracedecay project worktree association reassign --association <id> --work-item <id> --expected-version <n>
tracedecay project worktree cleanup inspect --worktree <id>
tracedecay project worktree cleanup status [--intent <id>|--worktree <id>]
tracedecay project worktree cleanup request (--intent <id>|--worktree <id> --preview-digest <digest>) --expected-lifecycle-generation <n> --confirm <token>
```

`discover` asks the daemon to ingest a bounded Git worktree-list/common-dir snapshot and reconcile hook/tool/CWD/thread/attempt/branch/commit/PR evidence. It records external creator/source provenance for agent/user/executor/Git/IDE/unknown origins; it never creates, provisions, moves, locks, or prunes a worktree. Candidate lists return deterministic score features, confidence, contradictions, and provenance. Associate/confirm/reject/reassign are idempotent expected-version commands; confirmation does not grant cleanup authority. Reconciliation/backfill is the same daemon discovery operation over an explicit evidence range, not a client database or filesystem scan.

`cleanup inspect` is the sole preview-as-evidence binding. Human/Markdown output leads with `Eligible | Blocked | Unknown | AlreadyAbsent`, triggers, external creator/owner evidence, cleanup-grant state, dirty/active/unpushed/unmerged/open-PR/shared-reference blockers, lifecycle/reference/policy versions, preview digest/expiry, branch-preserved disposition, and exact safe continuation. JSON carries the identical typed view. `cleanup request` consumes an application-minted `WorktreeCleanupIntentId`/inspect digest, separate cleanup grant, expected versions, confirmation, and idempotency. It returns the shared `OperationRef`; only the daemon re-probes and performs the external effect. No flag accepts a deletion path, `--force`, branch deletion, or a shell/Git cleanup command. Archive and verified merged-PR events can update eligibility or admit a policy-authorized request only when the separate grant and all proofs are current; blocked triggers remain diagnostics and are not put into task triage.

MCP generates the same `worktrees.discover|list|get`, `task_worktree_associations.list|diagnose|associate|confirm|reject|reassign`, and `worktree_cleanup.inspect|status|request` bindings plus read-only `tracedecay://v2/worktrees/{worktree_id}` and `tracedecay://v2/worktree-cleanups/{intent_id}` resources. The `tracedecay-context` profile may expose only bounded read/list/diagnose/inspect views. The `tracedecay-work` `orchestrator` profile may expose association decisions and cleanup request when its explicit grant ceiling permits; operator may expose discovery/reconciliation and cleanup request. Tool results use compact Markdown plus canonical `structuredContent`, pages retain cursors/coverage/watermarks, long blocker/reference evidence uses retrieval anchors, and long-running cleanup uses MCP progress/cancellation/protocol-task augmentation around the same `OperationRef`. No profile exposes a worktree-create tool or filesystem deletion instruction.

This taxonomy is a target navigation model, not authorization to rename everything at once. Each current path receives an alias/cutoff migration, generated replacement, and differential fixture. Frequently used native paths may stay short when their meaning is already canonical.

### 9.2 Help and discovery

Every command/tool help page is generated from the same binding row and includes:

- one-sentence task fit and negative guidance for commonly confused siblings;
- use-case/binding ID and lifecycle;
- availability and missing prerequisite;
- effect/auth/autonomy class;
- default scope and exact resolution behavior;
- parameters with type, units, default, range, enum, conflicts, and safe example;
- default/output formats, pagination, caps, coverage, and freshness;
- replacement/deprecation/cutoff;
- related CLI/MCP/API/SDK/dashboard/lab bindings;
- compact copyable invocations for human and canonical JSON use;
- a direct docs anchor.

Add:

```text
tracedecay help search <intent-or-terms>
tracedecay help show <capability-or-binding>
tracedecay help compare <binding> <binding>
tracedecay help available [--scope ...] [--format json]
tracedecay help schema <binding> [--format json]
```

Search covers names, stable IDs, old aliases, intents, task-fit phrases, inputs, outputs, product views, and skills. Unavailable capabilities remain visible with one reason and one remediation. “No result” is never used for unavailable/denied discovery.

Worktree lifecycle discovery explicitly distinguishes external-worktree observation, association decisions, cleanup-grant authority, read-only inspect/diagnose, and confirmed cleanup request. Help for every association/cleanup binding states “does not create a worktree”; request help additionally states that the daemon—not the CLI/MCP client—re-probes and performs the effect, preserves the branch, and refuses missing/current blockers.

### 9.3 Hints and skills

Policy/hook hints reference stable intent/capability/binding IDs and a generated summary. They never paste the full tool list. A hint may recommend the cheapest applicable binding, the exact scope it would use, and one command. Hint analytics record offered/acted/missed/corrected/suppressed outcomes by IDs, not free-text matching.

Managed skills declare versioned `UseCaseId` requirements and allowed effects rather than assuming an MCP tool name. Installation validates catalog digest and resolves each requirement through the pinned MCP profile when present, otherwise the generated CLI, then the documented official API when explicitly configured; an unavailable result names the exact missing profile/grant/prerequisite and safe install/command action. Skills plus CLI therefore remain complete when MCP is omitted. Skills cannot resurrect removed aliases, request a profile switch during a turn, invoke curation item approval paths, teach a generic invocation escape hatch, or teach surface-local scope/output behavior.

## 10. Scope, project, repository, worktree, and ref parity

### 10.1 Shared selector

Every scoped binding receives the exact plan-16 selector and resolved snapshot. Generated surface builders expose only legal selector fields for that use case. Resolution output includes:

- profile and privacy domain;
- selected project(s), repository/common-dir identity, checkout/worktree identity;
- branch/ref/commit/PR and graph/index generation;
- source/store shards and watermarks;
- ambiguity candidates, stale/locked/quarantined/unavailable coverage;
- default source and why it was legal;
- retry token/template when user selection is required.

Transport routing metadata is generated from a closed `RoutingFieldDescriptorV1`; there is no validation-bypass allowlist. A field is either a canonical typed selector/ID or a transport-only keyed locator. Human/runtime strings such as `project_root`, `cwd`, `hermes_home`, `storage_scope`, and `response_handle_project_root` cannot pass through untyped:

- display/explanation uses `SinkEligible<LogSafeText>` and never becomes a lookup key;
- lookup uses an opaque ID or `PrivacyDomainBoundLocatorDigest` computed inside the authorized resolver from a normalized path/source value;
- raw paths remain protected application inputs, never catalog metadata, metrics, logs, errors, response handles, help, or generated routing keys;
- handler/daemon/plugin code cannot add a field outside the catalog schema, and unknown routing fields fail validation before any store open;
- keyed-locator key epoch, privacy domain, source kind, normalization version, and scope are bound into the request/snapshot receipt; comparison across domains or key epochs is forbidden;
- profile-scoped and first-touch behavior is an explicit generated use case with the same resolver, authorization, problem, and coverage contract—not a CLI/MCP side list.

Each descriptor also carries one closed route class (`ProfileActivity`, `CurrentProject`, `ExplicitReadScope`, `ExplicitMutationScope`, or `RegistryDiscovery`), `requires_project`, legal implicit-current policy, and selector mutual-exclusion group. A single `ScopeRootV2::Profile` request is classified as `ProfileActivity` before CWD/session/host routing; a canonical query predicate may select Profile- or ZeroProject-owned activity rows, while an explicit canonical Profile+Project read is `ExplicitReadScope`. Only migration scalar user/profile aliases conflict with compatibility project fields. `RegistryDiscovery` executes without a project; explicit cross-project selectors are accepted only by cataloged read use cases; mutations require the exact authorized durable target; project-required calls fail rather than silently downgrade. These are generated from the same `UseCaseSpecV1` as CLI/MCP/API/SDK bindings—there is no tool-name allowlist in daemon, CLI fallback, or host plugin code.

Conformance tests enumerate every generated request field and fail if any field bypasses schema validation, lacks a safe-text/typed-ID/keyed-locator class, or reaches a log/error/presenter as a raw path.

### 10.2 Required regression corpus

Lock fixtures for:

- two registered projects with the same basename;
- one repository with base checkout plus several parallel worktrees;
- project marker and registry identity disagreement;
- selected and legacy shard conflict from the planning probe;
- explicit worktree path while MCP startup is scoped to another checkout;
- branch name existing in several repositories;
- ref missing from current graph but available in another generation;
- all-registered search returning a session that exact load must accept with the same scope;
- profile-wide All returning partial locked/stale stores;
- safe active marker differences between current CLI and MCP project output;
- credential-bearing remote URL present in source metadata but absent from search labels/output.
- explicit profile/user fact, LCM, memory-status, and message-search requests from `/`, a host-profile home, unrelated CWD, and project CWD; every binding reaches profile activity with no project flag/handshake/registration/initialization;
- every illegal migration scalar user/profile alias plus compatibility `project_id|project_path|project_root|project_scope|project_selector` combination, with identical typed invalid fields across transports, alongside valid canonical Profile+Project multi-root reads;
- host-profile home exclusion, a registered repository below that home, misleading ambient `HOME`/`HERMES_HOME`, and sequential/interleaved provider-session reset.

### 10.3 Labels and active markers

One typed label view feeds all human surfaces:

```rust
pub struct ProjectLabelViewV2 {
    pub project_id: ProjectId,
    pub display_name: CatalogSafeText,
    pub disambiguator: Option<CatalogSafeText>,
    pub repository_group: Option<RepositoryId>,
    pub checkout_kind: CheckoutKind,
    pub is_active: bool,
}
```

Repeated basenames use a safe parent-path/common-directory or explicit registry alias disambiguator. Remote URLs, credentials, query strings, usernames, and tokens are never labels. Markdown/terminal display `*` only as decoration derived from `is_active`; JSON carries the boolean. Missing registry returns the same outer typed collections, summary, cursor/cap, coverage, and active field shape as a populated registry.

## 11. Effects, auth, configuration, and autonomous curation

### 11.1 Effect classes

The closed effect enum is owned by plan 08's `effect.rs` as `EffectSpec.execution_mode`; this plan consumes it and defines no surface-local effect mode:

```rust
pub use tracedecay_tool_catalog::ExecutionModeV2;
```

Its exact variants are `ReadOnly`, `DirectCommit`, `ConfirmedDestructive`, `AutonomousPolicyEffect`, `ResumableWorkflow`, and `InternalHostLifecycle`. Each binding exposes grants, idempotency, expected-version policy, audit schema, progress/receipt shape, cancellation boundary, and recovery. CLI/MCP annotations are generated from this imported enum. Read-only hosts cannot obtain mutation bindings merely through a name alias.

MCP `readOnlyHint`, `destructiveHint`, `idempotentHint`, and `openWorldHint` are derived mechanically from this effect contract and egress policy. They are explanatory hints for trusted hosts, not enforcement inputs: every invocation still passes authenticated-principal, catalog grant, scope, plan-18 eligibility, and application policy checks. A mismatch between annotation and effect metadata blocks generation.

### 11.2 Direct configuration

Plan 20's complete generated `tracedecay config` tree is mandatory. `--json`/`--jsonl` are aliases for the shared format contract. Redactor, detector, privacy, retention, quarantine, source-field rules, scan schedule, false-positive policy, and non-disableable floor are visible and navigable in CLI and Brain Settings.

Routine `config set`, `unset`, batch commit, credential-reference bind, and forward restoration validate and commit directly with expected version/idempotency. Inline impact explains hot reload, restart, new session, rescan, reproject, reindex, migration, or unsupported state. There is no forced preview/apply/rollback ceremony.

### 11.3 Autonomous curation

Remove these V1 public semantics after outcome/status parity:

- `memory curate --apply`, `--llm`, and `--llm-ops`;
- automation-run `dry_run` as the only supported mode;
- fact proposal apply/reject queues;
- managed-skill draft/update/approve/install promotion queues produced by the curator;
- dashboard/API item approve/reject/apply/rollback controls;
- capability metadata suggesting curation candidates require human approval.

Expose only policy/configuration, schedule, budgets, authority, quality floors, run-now, pause/resume, circuit-breaker health, pin/protect/exclude, feedback, history, decisions, effects, outcomes, and incident diagnostics. Autonomous runs pin catalog/policy/config/eval digests. Human views explain what happened; they do not authorize each item after the fact.

The generated observation subtree is exactly `dirty-scopes list`, `admissions list`, and `admissions get`; it binds the three operations in Section 3 with the same typed pages/resources as HTTP/SDK/dashboard. Receipt representation shows every durable admission decision. Coalesced-skip-episode representation compresses repeated safe skips without inventing runs or losing the bounded evaluation-time/frontier range or latest evaluation anchor. Dirty scopes show pending versus consumed frontiers and shared retry/circuit/pause/quarantine/coverage state. `run-now` carries no force/ignore-digest flag and cannot cross the application's identical-terminal-input fence.

### 11.4 Confirmed destructive operations

Wipe, source cleanup, protected-data retirement, unsafe migration cutover, or external side effects can require an explicit confirmation token and current-version revalidation. Their names and receipts must describe the real effect. “Apply” and “rollback” are not generic framework verbs; use a domain command such as `migration cutover`, `migration recover`, or `project retire` where that is the actual operation. Move-symbol follows the same rule: `code.move_symbol.inspect` is a read-shaped preflight and `code.move_symbol.commit` is the confirmed mutation; both call plan 09's registered boundary and never select a transport-generic preview/apply mode.

Split-store consolidation follows the stricter plan-01/02/#425 workflow. The confirmation challenge binds both frozen source manifests, canonical platform locators, reservations, dual backup receipts, disposition plan, and destination. Start/resume never accepts an ambient current store; cutover is unavailable until the exhaustive verification view reports all required proofs, including remapped LCM source-edge integrity. Cancellation after an uncertain external/cutover effect enters typed reconciliation instead of claiming rollback.

Worktree cleanup follows plan 16's narrower lifecycle. Discovery/list/show/diagnose/inspect/status are read or observation/reconciliation bindings as cataloged; association decisions are direct expected-version commits; cleanup request is `ConfirmedDestructive` plus a resumable `OperationRef`. The confirmation challenge binds exact `WorktreeId`, Git admin/common-dir identity, lifecycle/association/reference/policy versions, separate cleanup grant, eligibility/preview digest, trigger set, blockers-empty proof, and branch-preserved effect. The daemon repeats the proof immediately before mutation. Missing, stale, dirty, active, unpushed, unmerged, shared, ambiguous, or changed evidence returns a typed blocker/conflict; it never downgrades to force or tells the client to delete a path.

## 12. Errors, status, stdout, stderr, and exit codes

### 12.1 One problem model

Application owns stable error codes. CLI and MCP map `ApplicationError` without parsing messages. `SurfaceProblemV2` is exactly the shared plan 09/10/17 problem shape — plan 10 §7.2's `ApiProblem` minus the transport-supplied RFC 9457 `problem_type`/`status` fields — with no field dropped or renamed; this plan adds only the Section 12.2 exit-class mapping:

```rust
pub struct SurfaceProblemV2 {
    pub code: ApplicationErrorCode,
    pub title: CatalogSafeText,
    pub detail: Option<CatalogSafeText>,
    pub instance: RequestId,
    pub retry: RetryDirective,
    pub restart: Option<RestartDirective>,
    pub current_binding: Option<BindingRef>,
    pub candidates: Vec<SafeCandidate>,
    pub invalid: Vec<InvalidField>,
    pub current_version: Option<AggregateVersion>,
    pub operation: Option<OperationRef>,
}
```

Human output leads with the problem and exact next action. JSON returns only the typed problem envelope. Raw provider/store/parser errors are logged through the safe observability path under the correlation ID and never copied into public detail.

### 12.2 Stable CLI exit classes

| Exit | Class | Examples |
|---:|---|---|
| 0 | success | complete query, accepted direct command, or explicitly allowed partial result |
| 2 | usage/validation | unknown command/flag/format, invalid typed input, missing required field |
| 3 | scope/identity | not found, ambiguous, identity split, ownership unresolved |
| 4 | auth/policy | unauthenticated, denied, privacy/payload denied |
| 5 | unavailable/freshness | capability unavailable, required refresh, all selected sources unavailable |
| 6 | conflict | expected version, idempotency, cursor/snapshot/protocol mismatch |
| 7 | retryable operation | transient dependency, rate/deadline, workflow still pending when synchronous completion was required |
| 8 | failed operation | durable workflow or confirmed command failed with a receipt |
| 9 | client incompatibility | stale protocol/catalog/binding requires update/restart |
| 70 | internal invariant | safe correlation ID only |
| 130 | cancelled | user cancellation/interrupt |

Useful partial results return 0 with `!coverage.is_complete()` unless the caller requests `--require-complete`, in which case the same response is written and exit 5 communicates the unmet contract. Empty complete and empty incomplete remain different in output.

### 12.3 Stream contract

- stdout contains only the selected result format;
- stderr contains human progress, retry notices, and diagnostics only when they are not part of machine output;
- `--format json|ndjson` never emits prose, ANSI, progress bars, warnings, or update notices on stdout;
- machine-relevant warnings, coverage, and progress are typed fields/events, not stderr-only information;
- `--quiet` suppresses optional human progress, never errors or result data;
- `--verbose` adds safe diagnostic events to stderr/human output and does not mutate JSON schemas;
- command modules return typed outcomes; they never call `process::exit`.

## 13. Pagination, cursors, truncation, and retrieval anchors

### 13.1 Collections

All bounded collections use the one `CursorPage<T>` envelope defined once in plan 17's contract IR (plan 17 §13.1) — `{ items, next_cursor, truncation, count_semantics, ordering }`; plan 10's `Page<T>` and this plan's `CursorPage<T>` are that same type, not variants, and neither plan restates its fields.

The earlier draft's `returned` and `page_limits` fields fold into this shape without information loss: returned-row counts are part of `count_semantics`, and the applied default/hard page limits travel in the `truncation` receipt.

Opaque authenticated cursors bind the canonical request fingerprint, access digest, resolved scope, schema/ranking/index/catalog versions, frozen watermarks, expiry, ordering cutoff, and per-shard positions. CLI exposes `--cursor`; SDKs expose bounded pagers. `--all-pages` is allowed only with explicit maximum pages/items/bytes/deadline. No SQLite transaction spans client think time.

Do not conflate three opaque token classes: an MCP discovery cursor paginates tools/resources/prompts over one catalog/list generation; an application cursor paginates semantic query rows over pinned data watermarks; a `RetrievalAnchorId` addresses a sanitized typed payload/evidence artifact. Each has a distinct codec, audience, expiry/retention, error, and restart directive. The MCP adapter unwraps/rewraps none of them and never accepts one where another is expected.

### 13.2 Output budgets

Pagination limits semantic row count. Presentation budgets limit human detail. Transport budgets limit encoded bytes/tokens. These are separate receipts. A renderer may reduce optional columns/detail according to a declared presentation ladder, but it cannot remove rows without the semantic page reporting it.

### 13.3 Retrieval anchors

When an individual eligible field or complete encoded response exceeds a transport cap:

- store the sanitized typed payload or eligible blob reference, not an unclassified rendered string;
- persist the canonical plan-01 `RetrievalAnchorRecordV1`, whose target binds the sanitized typed payload or eligible blob artifact to resolved scope, privacy domain, access-policy digest, source observations, snapshot, schema/catalog/data/projection versions, canonical request digest, payload-access state, provenance, expiry/durability, and retention class;
- return only that record's opaque `RetrievalAnchorId`; retrieval calls the cataloged generic `retrieval_anchors.resolve` use case, reloads `RetrievalAnchorRecordV1`, and reauthorizes the current principal rather than trusting a research entry ID or transport handle;
- expose exact omitted fields/bytes and retrieve binding;
- authorize and revalidate on retrieval;
- regenerate presentation from the typed payload where possible;
- preserve canonical retrieval-anchor identity across CLI/MCP/API when the same principal and scope permit;
- forbid a response handle as the only durable citation, saved-view, or export locator.

MCP renders the anchor as a `resource_link` to a generated `tracedecay://v2/retrieval/{anchor_id}` resource template plus the same ID in `structuredContent`. `resources/read` reauthorizes and resolves the canonical record through the application layer. V1 `rh_*` response handles are accepted only by the frozen differential harness until cutoff and are absent from V2 definitions, prompts, resources, hints, and live dispatch.

The frozen V1 harness includes PR #441's cross-project dereference failure: resolving a handle must preserve the original canonical resolved scope, explicit `project_path`/selector, TraceDecay profile, principal/grant, privacy domain, and source binding through the follow-up call. Reinterpreting the opaque handle under the active project or a host profile is a typed failure, never a fallback. V2 retrieval anchors make those bindings part of the record and reauthorize them; they do not inherit current CWD.

`memory_scope=profile` or equivalent V1 CLI/MCP spelling is migration syntax only. Current bindings lower to one Profile root plus the canonical `DeclaredScope::Profile` query predicate; active-project reads use `preset.knowledge.active-project-with-profile`. Projectless CLI/API calls authorize against the user-global TraceDecay profile without initializing a project or opening a separate database. A migration scalar user/profile alias plus any compatibility project field fails before resolution; a canonical Profile+Project multi-root read remains valid. The compatibility CLI process runs with a neutral working directory and sends no `--project` for the single-root route; this is defense in depth, not the authority boundary—the daemon application still resolves and authorizes the canonical request independently.

If storage is unavailable, return a typed `response_budget_exceeded` problem with a narrower request/cursor recommendation. Never emit `compacted_no_handle`, an invalid JSON prefix, or a false complete result.

### 13.4 Current regression cases

Fixture-lock:

- generic Markdown and JSON exceeding the current 15,000-character boundary;
- missing project root/handle cache;
- expired, missing, wrong-project, wrong-profile, wrong-access, and corrupt handles;
- selector-preserving cross-project auto-dereference versus active-project mismatch (FM-140), including identical behavior across CLI/MCP/API;
- LCM preflight/expand-query compaction tiers;
- multi-block MCP responses so notices do not hide payload/metrics;
- Unicode boundaries and Markdown/code-fence preservation;
- cursor page plus transport retrieval anchor in the same response;
- retrieval after privacy/config/catalog generation change;
- large field omitted while every collection row remains represented.

## 14. Status, coverage, freshness, missing registry, and partial data

Every result carries the application envelope. Human renderers always have bounded standard blocks for:

- resolved scope and active target;
- snapshot/watermark/fetched/indexed times;
- complete/partial/stale/locked/redacted/unavailable coverage;
- exact/estimated/lower-bound/sampled/capped/unknown counts;
- applied limits and next cursor/retrieval anchor;
- warnings and one safe remediation where actionable.

Rules:

- “No results” is rendered only when coverage proves the selected universe was searched completely enough to support that statement.
- Missing registry returns empty typed collections plus `registry_state=missing`, not an unrelated error string or absent fields.
- One unavailable shard may produce useful partial success; all unavailable returns a typed problem with per-source coverage.
- A stale graph result cannot be labeled current. Local semantic Git, live delivery state, and joined/reconciled conclusions remain distinct.
- Session/message/LCM/search reads never run provider catch-up. Stale/partial coverage may include the generated legal `capture.refresh` command and existing `OperationRef`; CLI/MCP/API/dashboard start or join that same operation and render identical progress/cancellation/terminal receipts.
- Active state is computed once in the application response and reused everywhere.
- Status commands and MCP status tools consume the shared `SystemStatusSnapshot`; they do not aggregate different component meanings under local booleans.
- Multi-machine status additionally renders `BrainId`, node role, authority/placement epoch, requested consistency, per-shard watermark/cache age/sync lag, pending local counts, conflicts/revocation, backup/recovery state, and unreachable/local-only/policy-excluded scopes. Pending/cache/replica state is never labeled canonical or complete.

## 15. Safe rendering and redaction

### 15.1 Sink boundary

Only sealed eligible types enter presentation or machine serializers. Compile-time lints reject public renderer parameters of `String`, bytes, `serde_json::Value`, raw provider records, raw config files, raw paths, or unsanitized errors.

Before output:

- authorize fields and payload classes;
- verify sanitization receipt/policy digest;
- apply field-level visibility and redaction state;
- escape Markdown, links, HTML, terminal controls, ANSI, OSC hyperlinks, CSV formulas, JSON/NDJSON line breaks, and logs according to the sink;
- scan generated examples/golden fixtures/docs and encoded output with plan-18 synthetic canaries;
- record counts/classes/rule IDs only, never candidate bytes.

### 15.2 Output-specific rules

- Markdown links allow only safe schemes and labels; local file anchors use authorized canonical paths or opaque IDs.
- Terminal output neutralizes control characters and honors `NO_COLOR`/`--no-color` without affecting meaning.
- JSON never interpolates pre-rendered Markdown/ANSI into semantic fields.
- NDJSON guarantees exactly one valid JSON object per line; embedded newlines are encoded.
- CSV escapes RFC-compatible fields and prevents spreadsheet formula execution for exported text.
- errors, labels, aliases, command examples, retry templates, and truncation instructions are safe catalog text.
- secret/credential availability uses typed present/missing/expired/locked state without values, prefix, length, equality fingerprint, URL, or username.

### 15.3 Redactor configuration surface

CLI/MCP help and Settings link every privacy/redactor use case to plan 20's canonical config key and effective policy view. Users can inspect and strengthen detector sets, thresholds, actions, structured field rules, retention, quarantine roles, scan schedules, and plugins. The non-disableable floor is visible but never writable. No tool-local `redact=false` or provider bypass exists.

## 16. Performance, latency, and token budgets

Every binding imports plan 08's `SurfaceBudgetV1` unchanged; plan 21 defines no surface-local budget type, and defaults are reviewed by use-case family rather than copied into handlers.

Targets on the reference machine:

- catalog exact binding lookup and generated dispatch: <=100 microseconds p95 excluding use-case execution;
- typed view to Markdown/terminal rendering for a default page: <=2 milliseconds p95 and <=2 MiB transient allocation;
- canonical JSON serialization for a default page: <=2 milliseconds p95;
- CLI parser/help startup after process launch: <=100 milliseconds p95 excluding daemon handshake;
- MCP initialize/version/capability/catalog handshake: <=100 milliseconds p95 with a warm daemon and no selected-store open; one generated list page <=10 milliseconds p95;
- MCP reader/writer/session routing adds <=1 millisecond p95 excluding application execution/rendering; a slow tool cannot block reading cancellation, ping, roots, or client responses;
- default MCP Markdown: <=4,000 estimated tokens soft budget; catalog-declared hard transport cap with cursor/retrieval recovery;
- default discovery result: <=1,000 tokens; one capability detail <=2,000 tokens;
- tool definition descriptions and examples: compact static metadata, with full docs retrieved explicitly rather than loaded into every host prompt;
- generated MCP profile ceilings are hard after tools-only fallback projection: `agent-core` 12/8k, `developer` 32/24k, `research` 24/18k, `task-worker` 24/16k, `orchestrator` 32/24k, `operator` 24/18k, and `admin-lab` 32/24k tools/estimated-definition tokens; generator failure is preferable to silent prompt growth;
- eager and deferred hosts run the same role-intent corpus; deferred loading may reduce presented definitions but cannot excuse a missing role-required binding, while an eager host may not exceed its pinned profile budget;
- task edit bundles declare total document/item/depth/body/diagnostic/diff/inline-MCP byte caps; a result above the inline cap returns one authorized resource link and compact continuation rather than truncating the document or placing it in tool text;
- default collection pages: normally 20–50 items, hard cap declared per use case; no unbounded `limit`;
- NDJSON/SSE queues: bounded items/bytes and explicit gap/resync behavior;
- MCP per-session requests, server requests, subscriptions, protocol tasks, progress events, and notifications each have reviewed count/byte/deadline caps; list-change/resource-update events coalesce by generation/URI and never form an unbounded queue;
- 1,000 repeated renders of the same typed fixture are byte-stable and leak no state;
- large result rendering scales linearly in returned rows/eligible bytes.
- Transport overhead is benchmarked separately from provider freshness work: the 30-project ≤60-second cold-history gate belongs to capture/application, while every search binding proves zero source opens, scanned bytes, cursor writes, or hidden refresh operations.

Benchmarks separately measure application execution, view construction, rendering, serialization, transport framing, truncation/anchor storage, and CLI/MCP overhead. Analytics record safe aggregate latency/bytes/tokens/format/truncation/cursor use by binding ID, never result text.

## 17. Compatibility, names, aliases, and deletion

### 17.1 Alias policy

- aliases are catalog rows with source name, canonical binding, exact semantic equivalence, introduced version, warning policy, cutoff, and docs replacement;
- an alias cannot change scope/default/effect/output or accept fields the canonical binding rejects;
- incompatible semantics get a new binding/use-case major, not an alias;
- old names may be searchable in help after cutoff but are not invokable;
- hidden provider commands are versioned internal bindings, not user aliases;
- bounded warning aliases are a CLI/documentation migration aid only. V2 MCP publishes one current name/schema/protocol epoch; it never accepts an old tool name, argument shape, response-handle envelope, or stale-session fallback after cutover. The plugin/client must upgrade or restart and receives the current binding in a typed incompatibility problem;
- current `query -> search`, `claude-install`, `claude-uninstall`, and `update-plugins` behavior receives explicit disposition;
- `removeall` is normalized with a cutoff rather than kept as permanent naming debt.

This plan owns the `CompatibilityDisposition` field contract that plan 08 embeds in every `SurfaceBinding` and plan 12 consumes in cutover receipts:

```rust
pub struct CompatibilityDisposition {
    pub action: CompatibilityActionV2,
    pub v1_surface: SurfaceKind,
    pub v1_names: BTreeSet<CatalogAlias>,   // every legacy name/alias/route this row covers
    pub replacement: Option<BindingId>,     // required for Replace
    pub alias_window: Option<AliasWindowV2>, // introduced / warn-from / cutoff protocol epochs
    pub differential_fixture: FixtureRef,   // V1/V2 semantic + presentation differential
    pub deletion_receipt: Option<ReceiptRef>, // set once the V1 surface is removed
    pub rationale: CatalogSafeText,
}

pub enum CompatibilityActionV2 {
    Keep,    // current name is already canonical; no alias window
    Rename,  // same semantics under a new canonical name; alias_window required
    Replace, // superseded by a different use case/binding; replacement required
    Remove,  // retired with no successor; deletion_receipt required at cutoff
}
```

Constraints: exactly one disposition exists per `(v1_surface, legacy name)` inventory row, and every frozen inventory row must reference exactly one; catalog validation rejects `Rename` without `alias_window`, `Replace` without `replacement`, `Remove` without a cutoff `deletion_receipt`, and any two dispositions claiming the same legacy name. Dispositions are catalog metadata: they live inside immutable `ToolCatalogSnapshot`s, are retained with them, and add no runtime store rows.

### 17.2 Cutover sequence

For each bounded context:

1. freeze source/runtime CLI and MCP inventories;
2. assign use cases/bindings/effects/output/presentation specs;
3. implement typed application views and errors;
4. generate shadow CLI/MCP bindings;
5. run V1/V2 semantic and presentation differential fixtures;
6. publish the new binding; keep warning-only exact aliases only on catalog-approved CLI surfaces during their bounded window, while MCP/plugin descriptors switch atomically and old MCP sessions fail current-version checks;
7. publish `CoreSkillsCli` as the portable baseline, generate independently installable `tracedecay-context`/`tracedecay-work` and explicitly opted-in `tracedecay-operator` facade components, compose zero/one/many without duplicate semantics, and pin one allowed profile/digest per connection; migrate a legacy full-catalog registration rather than wrapping it;
8. update installers/plugins/skills/docs/completion in one release; skills route by `UseCaseId` and retain CLI/API recipes when MCP is absent or intentionally narrower;
9. reject stale protocol/catalog/profile clients with one replacement/update/reconnect action;
10. remove aliases, every arbitrary hidden-binding CLI/MCP invoke bridge, and old dispatch from live surfaces at cutoff; retain only the generated schema-exact `tool <current-name>` CLI fallback;
11. delete handler-local args, allowlists, renderers, prints/exits, and docs;
12. retain only frozen inventory/replay fixtures until the data rollback window ends;
13. publish a deletion receipt proving zero live references.

### 17.3 Mandatory deletions

After final cutover delete or reduce to generated adapters:

- hand-maintained MCP definition, format-capable, project-selector, profile-tool, first-touch, and dispatch lists;
- the monolithic full-catalog default registration, per-host copied MCP installers, dynamic per-turn exposure/allowlists, and any generic invoke/god tool that bypasses an explicit generated binding;
- manual MCP JSON-RPC transport/types, hard-coded protocol version, degraded/replay server, static/raw-schema resources, daemon-side handshake/list-change parser, global pending notifications, and any old protocol/name/schema compatibility branch;
- native command-local output tables/JSON branches/progress/exit logic;
- `generic_md` over arbitrary JSON and handler-specific format parsing;
- irreversible truncation and handler-local LCM compaction envelopes;
- transport-routing argument validation bypass lists;
- duplicated project label/active/missing-registry renderers;
- V1 curation approval/apply/reject/draft/install surfaces;
- active aliases beyond cutoff;
- live legacy CLI/MCP fallback paths.
- any task-graph bulk editor, temporary-file janitor, document parser, validation schema, or submit transaction outside the shared application operation/structured-staging and canonical task-command path.

## 18. Generated documentation, schemas, completion, and parity matrix

Generate:

- complete CLI reference including hidden/internal appendix and alias cutoffs;
- complete MCP reference with negotiated protocol/capabilities, tools/input/output schemas/effects/auth/scope/formats/errors/limits, resources/templates/subscriptions, prompts/completion, progress/cancellation/tasks, sampling/elicitation boundaries, host availability, and examples;
- CLI↔MCP↔HTTP↔SDK↔dashboard use-case matrix;
- intent/task chooser and confused-tool comparisons;
- output-format and exit-code reference;
- scope selector examples for multi-repo/worktree/ref/All cases;
- cursor/retrieval/partial/error recipes;
- autonomous curation and configuration navigation guide;
- shell completions from legal names/keys/enums/layers, never secret values;
- machine-readable schema bundle and conformance fixture manifest.

Generated docs show source catalog/protocol/schema digest and version. CI regenerates twice, validates links/schema/examples, compares bytes, and requires a clean tree.

The parity matrix has one row per use case and one column per applicable binding. It compares canonical request/result/effect/error JSON first, then checked presentation differences. “Not exposed” requires a reviewed reason. No surface can be marked parity-complete from name or status-code equality alone.

## 19. Test and evaluation program

### 19.1 Inventory and generation

- recursive clap `CommandFactory` snapshot with every path in Section 3, aliases, hidden commands, flags, defaults, conflicts, validators, and output/effect state;
- source-definition/runtime-advertisement/handler/renderer/CLI/plugin comparison for every name in Section 4;
- explicit source 104 versus installed 103 drift fixture, including `ast_grep_search`, conditional `ast_grep_rewrite`, and `move_symbol`;
- deliberately add one uncataloged command/tool/format/scope allowlist entry and require named CI failure;
- deterministic generation across map order, locale, timezone, width, platform path separators, and host capability sets.
- snapshot `mcp-surface-profiles.json` and prove every profile is an explicit sorted `BindingId` set with the correct logical registration, effect/grant/host ceiling, eager tools-only fallback projection, count/token budget, and digest; inject a glob, cross-registration binding, implicit operator install, generic invoke binding, and over-budget definition and require named failures;
- assert exactly seven task edit-bundle use cases and one generated binding disposition per supported surface, with export/rebase/submit/delete absent from context/task-worker profiles and present only in `tracedecay-work`/`orchestrator`.
- assert the task-comment facade maps only to shared annotation use cases and `task_steering.submit|acknowledge|disposition` is the complete MCP mutation set; prove the disposition union maps bijectively to separate resolve/supersede/cancel use cases, then inject a conflated semantic command, missing/extra union case, comment-delivery, send-now, interrupt, adapter-selector, or fourth steering tool and require generation failure.
- assert one generated disposition for every worktree discovery/list/get, association list/diagnose/associate/confirm/reject/reassign, and cleanup inspect/status/request binding; inject create/provision/path-delete/force/branch-delete aliases and require catalog-generation failure.

### 19.2 Every-tool format conformance

For every readable MCP tool:

1. invoke with format omitted and assert valid compact Markdown plus schema-valid `structuredContent` from the same sealed typed view;
2. invoke `format=json`, decode the canonical typed schema from both `structuredContent` and the single JSON text block, and assert equality;
3. compare item identity/order/count/coverage/freshness/redaction/limits/active-target markers between modes;
4. assert the definition advertises exactly the implemented formats;
5. assert missing/empty/partial/error/large-result fixtures;
6. assert no raw JSON dump, dropped field, false empty state, double encoding, or irreversible truncation.

Give dedicated regression fixtures to `dsm`, `files`, `sessions_for`, `type_hierarchy`, and `workflows`; the current schema/render mismatch must fail before implementation. Keep the `unsafe_patterns` wrong-renderer/false-empty case as a permanent typed-view test.

Project list/search/context fixtures cover populated, empty, missing-registry, ambiguous, and partial states. JSON preserves the same outer collections, `summary`, `limit`, `truncated`, and active-state fields in every state; Markdown uses the same view to preserve active markers and safely disambiguate repeated basenames without displaying credential-bearing remotes. Tests never parse an omitted-format call as JSON.

For every mutation/internal tool, assert effect class, auth, idempotency/version, receipt, stdout/MCP problem, safe failure, and absence of unsupported human/JSON modes.

### 19.2A MCP protocol and host conformance

- Run the official MCP conformance suite and Inspector against stdio and enabled Streamable HTTP builds; save protocol revision, SDK version, feature flags, and result artifact.
- Fixture every lifecycle transition, incompatible version, capability subset, initialize ordering violation, ping, notification-with-no-response, unknown method/tool, malformed request, tool execution error, and graceful close.
- Prove generated tool/resource/template/prompt lists paginate over one catalog generation and emit only negotiated/coalesced list-change notifications.
- Prove `outputSchema` validates success/error `structuredContent`; default Markdown, explicit JSON, content annotations, embedded resource/resource-link, and safe large-result paths match canonical views.
- Exercise resource read/subscribe/unsubscribe/update/access revocation; prompt list/get; completion ranking/cap/redaction; roots list/change/ambiguous scope; and logging level isolation.
- Launch parallel slow calls, then deliver progress, cancellation, root/list changes, pings, and client responses while they run. Assert bounded queues, no stdout interleaving, deterministic races, and per-session isolation.
- Exercise optional/required protocol tasks through `tasks/list|get|result|cancel`; prove `McpTaskId` binds `OperationRef` and cannot resolve a plan-24 task/work-item/attempt ID.
- Negotiate sampling and elicitation independently. Assert no request when absent, no background-scout/curation sampling, no form-mode secret request, URL/state validation, and untrusted-result sanitization.
- Stdio tests forbid non-protocol stdout and unnecessary secret-bearing environment inheritance. Streamable HTTP tests cover Origin/Host, loopback, bearer/OAuth audience, protocol/session headers, SSE resumption without cross-stream replay, token passthrough rejection, rate limits, and reconnect after daemon drain.
- Maintain real host profiles for Codex, Claude Code, Cursor, Hermes, Gemini, OpenCode, Copilot, Roo/Kilo, Zed, and MCP Inspector. Record actual support for structured content, resource links, prompts, resources, completion, list changes, progress/cancellation, sampling/elicitation, and protocol tasks. Tools-only hosts remain functional; optional features never become assumed semantics.
- Run core-only, every allowed zero/one/many facade-component combination, explicit headless facade-only deployment, each logical registration/profile pair, eager tools-only fallback, and native deferred discovery against the same labelled role corpus. Assert role-required binding coverage, zero cross-profile/operator leakage, definition budgets, exact CLI/API fallback when absent, one thin integration-binary/private-daemon/application path, and no semantic difference by discovery mode.
- Change prompt intent during an open connection and assert the tool set/profile digest does not change. Exercise genuine catalog/availability/grant changes and assert coalesced `listChanged` never widens beyond the pinned profile; a requested profile change returns reconnect guidance.
- Exercise `task_graph.edit_bundles.export|get|validate|diff|rebase|submit|delete`: small inline and large resource-link reads, tools-only read fallback, Markdown-default and explicit-JSON diagnostics equality, resource immutability, stale base/rebase conflict, submit idempotency/atomicity, and orchestrator-only mutation authorization.
- Exercise worktree lifecycle tools/resources through context/work/orchestrator/operator profiles: external creator provenance, deterministic candidates, ambiguity, association revisions, cleanup-grant separation, blocker/reference pagination, inspect digest, request operation progress/cancel/resume/reconciliation, canonical structuredContent/Markdown equality, resource immutability, and zero create/provision/client-delete binding.
- Exercise steering resource/tools through task-worker and orchestrator profiles: direct submit, exact annotation-revision promotion, Plan-08/20 absolute/effective limits, two-controller CAS, duplicate idempotency, stale lease/fence/packet/graph rejection, acknowledge, disposition-tag mapping to applied/rejected resolve, higher-sequence supersede, and pre-delivery cancel, required completion fence, limit-lowering blocked remediation, advisory non-blocking, unsupported-boundary next-Turn fallback, reconnect, and late-terminal race. Assert accepted is never displayed as delivered and no profile receives comment injection or host-interrupt controls.
- Run the exact canonical search-evaluation family through generated CLI, MCP tools, MCP resources/templates, HTTP/SDK, and Search Quality UI metadata. Assert every read/command appears once, resources are read-only, list/get share typed views, fixture promotion has no invented fixture read, and no alias is emitted.
- Run `automation.dirty_scopes.list` and `automation.admissions.list|get` through generated CLI, MCP tools/resource template, HTTP/SDK, and dashboard metadata. Assert one catalog mapping per declared primitive and exactly the primitive set above, exact receipt/episode and frontier parity, generic retry/circuit/quarantine types, no fake run, no fourth operation/alias, and no `run-now` identical-input bypass.
- Run the generic experiment family through CLI, MCP tools/resources/progress/cancellation/protocol tasks, HTTP/SDK, and every Playground route. Assert one draft/create/run/status/cancel/resume/retry/minimize lifecycle, identical experiment/run/cell/stage/comparison/comparison-cell/reduction coordinates/anchors/receipts, `replay_stages.list(cell)` returning exact `ReplayTraceV1`, one top-level filtered list operation per resource, no per-lab run tool, no protocol-task/work-item ID confusion, and no MCP sampling without the explicit experiment model/egress grant.

### 19.3 Every-command CLI conformance

For every current and V2 command path:

- help/schema/completion agreement;
- valid/invalid/default/boundary/enum/unit arguments;
- canonical scope and ambiguity candidates;
- human/no-color/narrow-width/Markdown/JSON/table/NDJSON where supported;
- stdout/stderr cleanliness and exit class;
- cancellation, daemon unavailable, stale client, partial, conflict, and identity split;
- aliases before/at/after cutoff;
- no direct process exit from handlers;
- no transport `--dry-run` collision;
- `--json` and `--jsonl` exact alias equivalence;
- shell-safe examples and stdin/file payload behavior;
- current `cost --export` invalid-format nonzero regression.
- every `task-graph edit start|get|validate|diff|rebase|submit|clean` path, managed-directory versus caller-owned archive output, validate-only upload, pinned-candidate continuation, exact nonzero validation/conflict exits, and no public generic `invoke` command.
- every task-graph/project worktree discovery/list/association/diagnose/cleanup path, expected-version/idempotency/confirmation exits, `--cursor` pages, `--json` equality, operation status/reconciliation, absence of create/provision/path/force/branch-delete flags, and proof the CLI performs no Git/filesystem mutation itself.
- every `task-graph comments` and `task-graph steering` path, stdin/text bounds, exact comment-revision pins, sequence/lease/fence/packet/graph CAS, idempotent duplicates, required/advisory terminal behavior, `--watch` reconnect/gap handling, Markdown/JSON equality, and absence of send-now/interrupt/adapter/provider flags.

### 19.4 Cross-transport semantic parity

Run one canonical fixture per use case through the hermetic in-process application test oracle, the production daemon local-IPC protocol, its generated native CLI JSON command, the schema-exact `tool <current-name>` CLI JSON fallback where applicable, MCP JSON where the pinned profile exposes it, HTTP, Rust SDK, TypeScript SDK, Python sync/async SDK, and dashboard client where applicable. Compare after removing transport-only request/framing/timing fields; the oracle is not linked into production clients, and no arbitrary hidden-binding CLI/MCP invocation or direct-store bypass exists in the matrix:

- scope resolution and active label;
- rows/edges/facets/order/scores/count semantics;
- coverage/freshness/watermarks/redactions/retention/limits;
- cursor/restart/retrieval anchors;
- error code/retry/candidates/current version/operation;
- command effect/idempotency/audit/recovery;
- worktree creator/source provenance, association candidate/decision state, cleanup-grant authority, reference subjects/counts, eligibility/blockers, intent/receipt/failure/reconciliation, and branch-preserved effect;
- autonomous curation policy/run/outcome views;
- configuration effective values/provenance/impact;
- Git local/live/joined truth.

The parity matrix includes merged #445/#448 fixtures for user-scoped `message_search`, every LCM call, fact search/mutation/feedback, and memory status from neutral/host/unrelated/project working directories. It asserts identical route class, canonical scope-resolution ID, default source, authority, and truthful refresh coverage; zero project discovery/open/init or synthetic project flag; typed unavailable/incomplete coverage when profile activity is absent; identical `invalid_input` for every forbidden legacy project selector including `project_key`; and no raw CWD/path/`HERMES_HOME`/host-profile name in logs, metrics, or errors.

Session commands/tools use generated `SessionLocatorV1` unchanged. Canonical IDs hydrate directly; provider-native aliases require profile+provider, return the same zero/one/many canonical candidates as API/SDK/dashboard, and never let CLI/MCP first-match or encode a transport-only bare-ID shortcut.

### 19.5 Security, fuzz, and accessibility

- plan-18 positive/negative secret corpus through every format and error path;
- Markdown/HTML/link/ANSI/OSC/control/Unicode/bidi/zero-width/CSV-formula/JSON-line injection;
- repeated basename and credential-bearing remote URL fixtures;
- malicious catalog description, alias, field label, path, provider error, and retry text;
- response handle guessing, expiry, scope/auth replay, corruption, and path traversal;
- terminal widths 40/80/120/200, screen-reader/no-color/high-contrast copy behavior, and deterministic tables;
- explicit TTY versus pipe/file/tool capture, including `TERM=dumb` plus `NO_COLOR=1`; noninteractive status/help/results contain zero ANSI/OSC/cursor/half-block cells and stay within the declared output budget (FM-116);
- property tests proving renderer never changes semantic row membership.
- frontmatter edit-bundle fuzz for duplicate YAML keys, aliases/tags, multiple documents, depth/size/item limits, invalid UTF-8/control/bidi/Markdown/link content, unknown/read-only fields, secret canaries, JSON-pointer/source-span stability, and diagnostic rendering injection;
- managed-directory cleanup races: changed member/device/inode, symlink swap, missing/modified member, crash/restart/expiry, submit/clean idempotency, validation retention, and proof that caller-owned `--archive` is never removed.

### 19.6 Scale and fault matrix

- full catalog with thousands of extension bindings and fast help/search;
- thousands of projects/worktrees, concurrent agent readers/writers, partial shards, locked registry, daemon restart, stale protocol, disk full, handle-store failure, and cancellation;
- corrupt/missing optional semantic-Git enrichments, including the FM-117 invalid test-annotation database: CLI/MCP/HTTP/SDK return the same healthy direct diff plus typed partial coverage/rebuild action rather than surface-specific total failure;
- slow NDJSON/SSE consumers, bounded backpressure, gap/resync, and exact final coverage;
- renderer panic/serialization failure converts to safe invariant problem without partial stdout;
- identity-cutover conflict returns the same candidates/remediation through CLI/MCP/HTTP.
- 64 concurrent CLI/MCP/API/dashboard freshness requests join one operation; leader death, cancellation, partial source failure, and terminal error are byte-equivalent semantic outcomes, and no waiting adapter reports completion from a process-local notification alone.
- edit bundles at maximum item/edge/body size, concurrent plan-version change, active lease, cycle/dangling reference, validation cancellation, rebase conflict fanout, submit kill points, staged-payload GC, and indexed expiry cleanup without a full-bundle scan.
- worktree inventory/reconciliation across thousands of Git admin records and tasks; duplicate/reordered hook/CWD/attempt/PR/archive/merge observations; ambiguous correlation; stale/orphan horizons; dirty/active/unpushed/unmerged/shared/unknown blockers; association/eligibility CAS races; identity change after inspect; daemon crash before/after external effect; and deterministic intent/receipt recovery without branch deletion or task-triage creation.

## 20. Implementation slices inside the existing master program

These are sub-slices of existing PRs, not a separate architecture track.

### PR 1/3 companion — frozen surface/output audit

- Generate the recursive CLI and full MCP source/runtime inventories.
- Record every current inconsistency from Section 2 and every row from Sections 3–4.
- Freeze all seven integration aliases, the nine-command canonical `integration` tree, and every provider hook launcher lowering to the hidden stdin-only `host-event ingest` binding.
- Add source/runtime/release drift and TraceDecay identity-conflict fixtures.

### PR 4/9 companion — output-safe domain and store contracts

- Add presentation/retrieval IDs, count/order/page/anchor/output budget contracts where not already owned.
- Persist sanitized typed retrieval anchors and expiry/audit metadata without adding renderer behavior to stores.

### PR 22A companion — binding and presentation specs

- Extend the capability catalog with formats, presentation IDs, effect modes, exit classes, stream/export support, cursor/anchor, budgets, component-set/install-scope/profile selection, `McpSurfaceProfileV1`, and the complete MCP primitive/capability/task/subscription/completion contract.
- Generate `mcp-protocol.json`, `mcp-surface-profiles.json`, tools with input/output schemas, resources/templates, prompts, completion eligibility, list generations, CLI schemas/help/docs, and parity matrix; reject every duplicate allowlist, profile glob, cross-trust binding, generic invoke tool, or budget overflow.
- Catalog the exact task edit-bundle family and profile visibility, including the large-bundle resource-link contract and typed diagnostics/diff/receipt presentations.
- Catalog the shared-annotation task-comment facade, full CLI steering tree, compact three-tool MCP steering surface, attempt resource, grants, safe-boundary/delivery dispositions, and required/advisory completion-fence presentation without adding another event stream or notification family.
- Catalog the exact external-worktree discovery/association/diagnose and cleanup inspect/status/request family, effect/grant/profile ceilings, cursors/SSE links, blockers/receipts, and forbidden create/provision/client-delete aliases.
- Catalog `integrations.list|get|diff|status|install|update|repair|uninstall|verify`, their admin/effect/operation/output contracts, operator-only optional MCP exposure, and the internal `host-event ingest` binding; generate no legacy alias tool.

### PR 24A companion — sealed typed application views

- Replace raw result maps with domain-clustered transport-eligible views.
- Standardize coverage, empty/missing/partial states, labels, errors, and operations.
- Carve autonomous curation and direct configuration out of any generic preview/apply command abstraction.
- Add sealed integration inventory/detail/difference/status views and the one application-owned operation lifecycle; root composition remains only the deployment/probe/config effect port.
- Reuse the generic operation and structured-staging kernel for protected task edit bundles; compile submit into canonical plan/work-item/edge commands and add no document-specific scheduler, retry engine, or task store.
- Reuse shared annotations for task comments and the canonical task journal/outbox plus application command/CAS path for steering. Add no comment store, steering journal, scheduler, model channel, or client delivery retry.
- Reuse canonical task relations/events, generic operations, outbox, audit, and retention for worktree association and cleanup lifecycle views; add no cleanup database/service, triage work item, or client mutation port.

### PR 24E0–24E8 companion — daemon-only execution, pure presentation, and generated adapters

- Land PR 24E0's split binaries first: CLI and every MCP profile are mandatory authenticated daemon clients for all business/query operations and contain no embedded application/store fallback. Only manifest/service-manager lifecycle bootstrap may start/status/recover a missing daemon, and it cannot claim database contents.
- Land the mandatory root `v2::presentation` module and its plan-19 module-lint edge; delete all other root/handler-local human renderers as each domain cuts over.
- Pin the official Rust SDK/current stable protocol, land the connection/session state machine, generated service router, per-session notification/cancellation/subscription state, stdio transport, and optional secured Streamable HTTP transport.
- Replace copied/full-catalog host installers with the generated skills+CLI baseline and optional context/work/operator registration façades over that one adapter; pin profile/digest at initialize and require reconnect to change it.
- Generate the exact `tracedecay integration` tree, hidden stdin-only host-event adapter, alias cutoff shims, admin HTTP/SDK/dashboard parity, and optional operator MCP bindings from one catalog/application implementation; cover component-set and zero/one/many registration fixtures.
- Cut MCP tools, structured results/errors, resources/templates, prompts/completion, roots/logging/list changes, progress/cancellation, then protocol tasks and explicitly authorized sampling/elicitation; each capability lands only with its conformance/host matrix.
- Cut CLI and MCP semantic domains over one at a time with application/presentation differential tests; wire behavior remains transport-native.
- Normalize stdout/stderr/exits, format switches, scope builders, help, cursors, and retrieval anchors.
- Generate worktree lifecycle CLI/MCP bindings and ticket/API/SSE parity together; daemon-only cleanup request remains one operation across progress, polling, cancellation, and recovery.
- Generate comment/steering CLI, compact MCP, HTTP/SDK, dashboard, and canonical task-subscription parity together; safe-boundary host delivery remains plan 07's daemon adapter concern.

### PR 24D/API companion — official clients and documentation

- Generate machine schemas, cross-surface links, conformance runner, NDJSON/SSE clients, and complete reference.
- Prove direct API callers receive the same semantics without CLI/MCP scraping.

### PR 25/31 companion — Settings, command palette, and labs

- Make all configuration/redactor controls navigable in Brain Settings and CLI.
- Add output/scope/error/catalog inspectors and synthetic replay fixtures.
- Show autonomous curation history/outcomes without item authorization controls.

### PR 33–36 companion — shadow, backfill, and cutoff

- Run real-project/worktree/session differential corpora plus synthetic privacy fixtures.
- Publish accepted presentation differences, client cutoff, and migration receipts.
- Make generated V2 bindings the only live surface.

### PR 37 companion — deletion gate

- Delete `src/mcp/transport.rs`, manual JSON-RPC request/response/error types, degraded/replay server, static resource array/raw schema resource, hand-maintained definitions and dispatch/routing/format/profile/first-touch lists, renderer-side result mutation, project-local response-handle protocol, daemon handshake/list-change interception, local exits, expired aliases, and curation proposal surfaces after their generated replacements pass.
- Require zero uncataloged command/tool, zero semantic duplicate, zero raw-Value renderer, zero irreversible truncation, and zero V1 live fallback.

## 21. Verification commands and artifacts

Implementation verification from the repository root includes:

```bash
cargo test -p tracedecay-tool-catalog complete_inventory transport_parity
cargo test -p tracedecay-tool-catalog mcp_protocol_generation
cargo test -p tracedecay-tool-catalog mcp_surface_profiles task_graph_edit_bundle_bindings
cargo test -p tracedecay-application surface_views
cargo test --test presentation_v2
cargo test cli_mcp_http_sdk_parity
cargo test mcp_protocol_conformance
cargo test mcp_host_profiles
cargo test task_graph_edit_bundles
cargo nextest run --workspace --no-fail-fast
(cd dashboard && npm test -- settings command-palette)
(cd dashboard && npx playwright test settings output-inspector autonomy)
gitleaks git --redact --no-banner
gitleaks dir generated docs dashboard packages python tests --redact --max-archive-depth 2
```

Required artifacts:

- source/runtime CLI and MCP inventory manifests with digests;
- full disposition and parity matrices;
- generated help/schema/docs/completion hashes;
- generated MCP install-mode/logical-registration/profile manifest and eager/deferred host budget report;
- task edit-bundle schema, lifecycle, diagnostics/diff, large-resource-link, atomic-submit, and managed-cleanup receipts;
- official MCP conformance/Inspector reports for stdio and enabled Streamable HTTP plus per-host capability profiles;
- semantic and presentation golden fixtures;
- current/V2 differential report by binding/use case;
- performance/token/byte/latency benchmark report;
- secret/injection/accessibility/fault receipts;
- alias cutoff and stale-client conformance report;
- final deletion receipt.

Plan-file checks before handoff:

```bash
test "$(rg -c '^```' docs/plans/tracedecay-v2/21-cli-mcp-tool-surface-and-output-unification.md)" -ge 2
rg -n 'TB[D]|TO[D]O|FIXM[E]|PLACEHOLDE[R]|00x[x]|implement late[r]|fill i[n]' docs/plans/tracedecay-v2/21-cli-mcp-tool-surface-and-output-unification.md
gitleaks dir docs/plans/tracedecay-v2 --redact --no-banner
```

The fence test is supplemented by a parser that requires an even fence count and validates local Markdown links.

## 22. Definition of done

- [ ] Every current visible/hidden/aliased CLI path and all 104 source MCP definitions have one reviewed use-case/binding/lifecycle disposition.
- [ ] Source, runtime, installed plugin, generated docs, and tests agree on advertised/available tool sets; host-conditional absence has a typed reason.
- [ ] One catalog generates names, descriptions, params/defaults, scope, effects, auth, formats, help, docs, completion, and parity fixtures.
- [ ] The current stable MCP revision and official Rust SDK are pinned and conformance-tested; initialize/version/capability/initialized/drain/reconnect state is enforced before application/store access.
- [ ] Catalog generation owns tools with input/output schemas, resources/templates, prompts, completion, annotations, task support, subscriptions, and list generations; every advertised capability has an implementation and host fixture.
- [ ] Skills plus CLI are semantically complete without MCP. Optional MCP uses one adapter and only `tracedecay-context`, `tracedecay-work`, or explicitly opted-in `tracedecay-operator`, with a fixed explicit profile/digest and profile∩host∩grant∩authorization visibility.
- [ ] Profile sets contain no globs or generic invoke/god tool, stay inside eager-host count/token budgets, never switch per turn, and use `listChanged` only for real changes inside the pinned profile; deferred tool search is optional.
- [ ] `task_graph.edit_bundles.export|get|validate|diff|rebase|submit|delete` is the sole complex bulk-edit family; CLI exposes `task-graph edit start|get|validate|diff|rebase|submit|clean`, only validate uploads a sharded directory/archive, later stages consume one pinned `TaskGraphEditCandidateRefV1`, large MCP bundles return authorized resource links, and only `tracedecay-work`/`orchestrator` exposes edit mutations.
- [ ] Task comments are the shared annotation family and never imply delivery. The full CLI exposes separate resolve/supersede/cancel commands; compact MCP `submit|acknowledge|disposition` maps a closed tagged union to those distinct semantic use cases. All surfaces preserve exact attempt/lease/fence/sequence/packet/graph/idempotency, Plan-08/20 limit, and delivery/ack/disposition receipts; required unresolved/unknown/limit-blocked state fences terminal integration, advisory does not, and no send-now/interrupt/provider selector exists.
- [ ] Frontmatter Markdown has closed schemas and source-spanned typed diagnostics; validate/diff are nonauthoritative, rebase reports conflicts, submit is one expected-version/idempotent owner-shard transaction, and safe cleanup never deletes caller-owned or replaced files.
- [ ] Externally created worktrees have generated discover/list/get and task-association list/diagnose/associate/confirm/reject/reassign bindings with provenance/confidence/ambiguity/reconciliation; no create/provision-worktree capability exists.
- [ ] Cleanup inspect/status/request has exact CLI/MCP/HTTP/SDK/dashboard parity, stable cursor/output/SSE semantics, separate cleanup-grant authority, dirty/active/unpushed/unmerged/shared/unknown blockers, CAS/idempotency, daemon re-probe, operation receipts/failures/reconciliation, and branch preservation. Blocked triggers are diagnostics, not task triage.
- [ ] The canonical search-evaluation reads/commands have exact generated CLI/MCP/resource/HTTP/SDK/Search Quality UI parity, with read-only resources and zero invented aliases/use cases.
- [ ] Automation dirty-scope/admission list/get reads have exact generated CLI/MCP/resource/HTTP/SDK/dashboard parity; coalesced skip episodes preserve evaluation-time/frontier tuples and stable anchors, current/considered/consumed/included frontiers remain explicit, shared health/reconciliation state is not forked, and run-now cannot bypass identical-input fencing.
- [ ] The generic experiment draft/create/run/status/cancel/resume/retry/minimize and experiment/run/cell/stage/comparison/comparison-cell/reduction list/get reads have exact CLI/MCP/HTTP/SDK/dashboard parity; every lab is a catalog evaluator, not a second tool or lifecycle family.
- [ ] One application use case executes each semantic operation; CLI/MCP contain no store/query/policy behavior.
- [ ] Machine JSON is canonical typed semantic data across CLI/MCP/HTTP/SDK and is never a double-encoded transport wrapper.
- [ ] MCP defaults to compact Markdown; CLI defaults to deterministic human output; all explicit formats are schema-advertised and tested.
- [ ] No public renderer accepts raw JSON/string payloads or can silently drop rows/fields/coverage.
- [ ] Every collection has deterministic order, authenticated cursor, caps, and resumable SDK/CLI behavior.
- [ ] MCP discovery cursors, semantic query cursors, retrieval anchors, resource URIs, protocol task IDs, and plan-24 task identities remain typed and non-interchangeable.
- [ ] Every transport-size truncation is explicit and recoverable, or returns a safe budget error; no `compacted_no_handle` remains.
- [ ] Missing registry, active marker, repeated basename, partial/stale/locked/redacted, and empty states have stable typed shapes.
- [ ] stdout/stderr and exit codes are stable; command modules do not call `process::exit` or print machine-breaking prose.
- [ ] All project/repository/worktree/ref/profile/provider selection uses unchanged `ScopeSelectorV2`; ambiguity never first-matches.
- [ ] Redactor and every non-secret configuration key are fully visible/navigable in Brain Settings and generated CLI/MCP/API/SDK surfaces, subject to the non-disableable floor.
- [ ] Configuration edits validate and save directly; no routine preview/apply/rollback ceremony exists.
- [ ] Curation/self-improvement is fully autonomous; no per-item preview/approve/reject/apply/install/rollback binding or UI control exists.
- [ ] Destructive non-curation operations retain explicit confirmation, audit, idempotency, and recovery where required.
- [ ] Progress/cancellation, resource subscriptions/list changes, roots, logging, protocol tasks, and explicitly authorized sampling/elicitation operate concurrently under per-session auth/backpressure; a tools-only host still works without semantic fallback.
- [ ] Stdio emits only MCP messages on stdout. Enabled Streamable HTTP passes Origin/Host/session/protocol/auth/resumption tests, and no token is accepted in a URL/tool argument or passed through.
- [ ] Help, hints, and skills route by stable IDs and never teach stale aliases, duplicated scope logic, or full-catalog spam.
- [ ] Markdown, terminal, table, JSON, NDJSON, SSE, exports, errors, docs, and fixtures pass privacy/redaction/injection gates.
- [ ] Performance, token, deterministic generation, cross-transport parity, scale, fault, and accessibility gates pass.
- [ ] Hand-maintained definition/format/scope/routing lists, manual JSON-RPC types/transport, degraded/replay server, raw-schema resource, result mutation, response-handle protocol, daemon handshake fork, raw renderers, local output branches, proposal queues, expired aliases, and V1 live fallbacks are deleted.
- [ ] The final plan set tells one flow: cataloged intent -> generated binding -> shared scope/auth -> application use case -> typed view -> safe renderer/serializer -> cursor/coverage/anchor -> audit/analytics, with no parallel semantic path.
