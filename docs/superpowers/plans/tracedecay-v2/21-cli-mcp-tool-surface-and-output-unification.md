# TraceDecay V2 CLI, MCP, Tool Surface, and Output Unification Plan

> **For agentic workers:** implement this plan only inside the existing V2 program. Do not create a parallel command system, renderer stack, scope resolver, error model, or configuration registry.

**Goal:** Replace TraceDecay's independently evolved CLI commands, MCP tool definitions, routing allowlists, output switches, raw-JSON renderers, pagination conventions, response truncation, help text, and compatibility aliases with one generated semantic surface. Every current command and tool receives an explicit keep/replace/remove disposition; every surviving binding invokes one application use case and renders one typed result consistently.

**Architecture:** [`08-tool-catalog-crate.md`](08-tool-catalog-crate.md) owns stable capabilities, use cases, binding IDs, effect metadata, and generated surface definitions. [`09-application-crate.md`](09-application-crate.md) owns execution and canonical `ApplicationResponse<T>` views. A small pure `tracedecay-presentation` crate converts only sealed typed views into a transport-neutral document model and renders Markdown or terminal text; canonical JSON serializes the same view without passing through the document model. Generated CLI and MCP adapters resolve the same [`ScopeSelectorV2`](16-cross-project-repository-worktree-scope.md), call the same application port, map the same errors, and apply catalog-declared output, pagination, privacy, and budget policy. HTTP/SDK JSON, NDJSON, and SSE remain owned by plans [`10`](10-api-crate.md) and [`17`](17-official-public-api-and-sdks.md).

**Normative dependencies:** [`01-domain-crate.md`](01-domain-crate.md), [`05-query-crate.md`](05-query-crate.md), [`08-tool-catalog-crate.md`](08-tool-catalog-crate.md), [`09-application-crate.md`](09-application-crate.md), [`10-api-crate.md`](10-api-crate.md), [`12-root-compatibility-migration.md`](12-root-compatibility-migration.md), [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md), [`17-official-public-api-and-sdks.md`](17-official-public-api-and-sdks.md), [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md), [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md), [`20-configuration-control-plane.md`](20-configuration-control-plane.md), [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md), [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md), and [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md).

---

## 1. Contract lock

1. A user intent maps to one `UseCaseId`; a concrete exposure maps to one `BindingId`. CLI, MCP, HTTP, SDK, dashboard, hook, skill, and automation names are bindings, never separate implementations.
2. The capability catalog is the sole source of names, descriptions, aliases, parameter schemas, defaults, scope requirements, effect class, auth grants, output formats, pagination, budgets, examples, availability, lifecycle, and replacement instructions.
3. The application layer returns a sealed typed semantic view. CLI/MCP renderers cannot query stores, infer missing fields, patch labels, reinterpret errors, or traverse raw `serde_json::Value`.
4. MCP human output defaults to compact Markdown. CLI human output defaults to deterministic terminal text. Machine callers opt into canonical JSON explicitly. JSON never means “the JSON-RPC wrapper containing a string containing JSON.”
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

Publication refresh is `origin/master` `6c4b8b91dad2efdcaefab0153475287f37c2caee`: merged #407 removes Hermes-local profile/tool silos, #420 makes daemon-proxy authority precede local store open and preserves per-request reconnect/no-write-replay semantics, #422 negotiates MCP `tools.listChanged` with bounded per-generation refresh, #423 fixes fact-rank/counters, and #424 shares exact analytics aggregates across MCP/dashboard. Open #418 remains a refresh input. V2 absorbs these into generated bindings/handshakes/views rather than adding adapter-local inventories or renderers.

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
- at least one invalid output mode (`cost --export <unknown>`) prints an error locally instead of sharing typed validation and a guaranteed nonzero exit.
- limits and ordering are handler-local; many lists return a cap or `truncated` boolean without an opaque resumable cursor.

Every item above becomes a fixture before V2 adapter work begins.

## 3. Complete current CLI inventory and required disposition

The generated recursive clap inventory is authoritative. The following human matrix is an audit anchor and must remain complete until the V1 cutoff.

| Current path family | Every current path | V2 disposition |
|---|---|---|
| Core index and status | `init`, `sync`, `status`, `list`, `wipe`, `gitignore` | Bind typed project enrollment, capture/index workflow, system status, registry query, confirmed destructive retirement, and configuration use cases. Remove local output/scope logic. |
| Generic tool bridge | `tool`, generated `help` | Keep a generated direct binding bridge, but make canonical input/output semantics unambiguous; discovery comes from the catalog. |
| Agent integration | `install`/`claude-install`, `reinstall`, `update-plugin`/`update-plugins`, `uninstall`/`claude-uninstall` | Replace aliases at cutoff with cataloged host-integration workflows, effect receipts, safe progress, and one current name. |
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
| Automation runs | `automation run memory-curation`, `session-reflection`, `skill-writing`; `automation runs list`, `view`, `artifact` | Remove dry-run/proposal semantics. Keep autonomous run-now and read-only run/artifact/outcome views with pinned policy/config/eval digests. |
| Managed skills | `automation skills list`, `view`, `draft`, `update`, `approve`, `disable`, `archive`, `restore`, `install` | Remove per-item authoring/approval/promotion bindings from autonomous evolution. Replace with inventory, history, decisions, outcomes, authority, pin/protect/exclude, health, and feedback. |
| Fact proposals | `automation facts list`, `view`, `apply`, `reject` | Remove proposal queue and item mutations. Replace with autonomous decision/effect history and policy/quality controls. |
| Store migration | `migrate plan`, `export`, `apply`, `verify`, `reconstruct`, `registry-gc`, `rollback`, `cleanup-sources` | Map to typed inventory, export, verified migration workflows, confirmed destructive cleanup, and recovery. Curation autonomy does not remove system migration safety. Rename V1 ceremony terms where the V2 workflow model supersedes them. |
| Hidden extraction | `extract-worker` | Internal host binding only; generated protocol/version handshake and machine-only output. |
| Hidden Claude hooks | `hook-pre-tool-use`, `hook-prompt-submit`, `hook-stop`, `hook-claude-session-start`, `hook-claude-post-tool-use`, `hook-claude-subagent-start` | Generated provider descriptor and internal hook bindings; never hand-authored clap commands. |
| Hidden Kiro hooks | `hook-kiro-pre-tool-use`, `hook-kiro-prompt-submit`, `hook-kiro-post-tool-use` | Same internal hook contract. |
| Hidden Cursor hooks | `hook-cursor-subagent-start`, `hook-cursor-post-tool-use`, `hook-cursor-before-submit-prompt`, `hook-cursor-pre-compact`, `hook-cursor-after-file-edit`, `hook-cursor-session-start`, `hook-cursor-session-end`, `hook-cursor-after-shell`, `hook-cursor-workspace-open`, `hook-cursor-stop` | Same internal hook contract. |
| Hidden Codex hooks | `hook-codex-session-start`, `hook-codex-user-prompt-submit`, `hook-codex-subagent-start`, `hook-codex-post-tool-use`, `hook-codex-post-compact` | Same internal hook contract. |

Plan 24 adds generated V2-only `initiative`, `plan`, `task`, `executor`, `scheduler`, `task-view`, and `task-graph` groups plus audience-filtered executor lifecycle bindings. They are not shoehorned into legacy `automation`, `projects`, `branch`, or generic `tool` semantics. Every CLI/MCP/HTTP/SDK/dashboard exposure maps to one catalog use case and sealed task/plan/executor view; compact output always retains canonical IDs/versions, blockers, coverage, packet/lease/route status, anchors, and legal next actions. Fence proofs, credentials, protected logs, and private sibling content never render.

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

## 5. Stable semantic identity and generated surface manifests

### 5.1 IDs

Use plan 08's identity model without surface-derived business identity:

```rust
pub struct CapabilityId(ValidatedId); // capability.code.search
pub struct UseCaseId(ValidatedId);    // usecase.code.search-symbols
pub struct IntentId(ValidatedId);     // intent.code.find-symbol
pub struct BindingId(ValidatedId);    // binding.mcp.search
pub struct PresentationId(ValidatedId); // presentation.code.search-results
```

All five ID kinds follow plan 08 §8's grammar exactly — `usecase.<domain>.<verb-noun>`, `intent.<domain>.<task>`, `binding.<surface>.<stable-name>`, and `presentation.<domain>.<view>` (registered in plan 08's `id.rs`). Versions are separate SemVer fields; IDs never embed v1/v2 or transport names except BindingId.

One use case may have native CLI, generic CLI bridge, MCP, HTTP, SDK, dashboard, hook, and skill bindings. A binding declares only transport syntax and presentation support. It cannot alter default scope, query semantics, ordering, coverage, effect, or errors.

### 5.2 Canonical generated artifacts

Plan 08 §6's `generated/` filename set is the single canonical artifact home; this plan renders only from those files and adds its surface artifacts to that same set:

```text
generated/                  # canonical home and names: plan 08 §6
├── catalog.json            # capabilities + use cases
├── cli-bindings.json
├── cli-command-tree.json
├── mcp-tools.json          # MCP binding rows + emitted tool definitions
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
- an active alias past cutoff;
- generated output drift or non-deterministic order.

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
| Human presentation | `tracedecay-presentation` | native-command `println!` layouts and raw-Value Markdown |
| HTTP/NDJSON/SSE envelopes | API plan 10 | CLI/MCP transport inventing stream protocols |
| Configuration | plan-20 registry/application | CLI-only flag or dashboard-only setting |
| Privacy eligibility/redaction | plan 18 | output-specific string scrubbing |
| stdout/stderr/exit mapping | generated CLI adapter | command-module process exits |
| MCP result/problem mapping | generated MCP adapter | handler-local status prose |

### 6.1 `tracedecay-presentation` scope

Add a small pure crate only because CLI, MCP, documentation snapshots, and the conformance runner need byte-identical human rendering without importing root transport code:

```text
crates/tracedecay-presentation/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── document.rs
│   ├── spec.rs
│   ├── budget.rs
│   ├── markdown.rs
│   ├── terminal.rs
│   ├── table.rs
│   ├── labels.rs
│   ├── problems.rs
│   ├── progress.rs
│   └── escape.rs
└── tests/
    ├── golden.rs
    ├── parity.rs
    ├── width.rs
    ├── injection.rs
    └── secret_canary.rs
```

Allowed imports: domain safe value types, application public view types, generated presentation descriptors, Unicode width helpers, and pure serialization/test libraries.

Forbidden imports: stores, queries, policy execution, hooks, providers, Axum, rmcp, clap parsing, SQL, Git/network clients, filesystem/process access, environment/time reads, and `serde_json::Value` in public renderer APIs.

If implementation proves the root binary is the only renderer consumer, keep the same boundary as a sealed root module rather than adding a crate. The API and ownership remain identical; no second presenter is allowed.

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
```

Every content-bearing field is a plan-18 eligible wrapper or an explicit redacted/denied/unknown variant. `ApplicationResponse<T>` carries scope, snapshot, coverage, freshness, redactions, retention, limits, and warnings once.

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
--consistency eventual|frozen|at-least-watermark
--limit <n>
--cursor <opaque-cursor>
--fields <generated-field-set>
--detail compact|normal|full
--no-color
--quiet
--verbose
```

Ergonomic flags are lossless builders for `ScopeSelectorV2`. They are mutually exclusive with `--scope` when they overlap. Generated help shows the canonical selector before invocation. Mutations require an explicit durable target; queries use a default only when the catalog declares it, such as profile-wide `AllAuthorized` for Brain.

No global flag shadows a semantic input field. Transport-side schema validation becomes `tracedecay invoke <binding> --validate-input`; semantic `dry_run` is removed in favor of the use case's declared execution mode. Debugging the raw transport envelope uses a hidden developer command, never public `--json`.

### 8.3 Canonical MCP result

MCP bindings accept one generated request schema plus a generated `format` enum where human rendering is supported. The adapter returns:

- `content`: compact Markdown text in default mode;
- `structuredContent`: canonical typed result when supported by the negotiated MCP protocol;
- JSON text in `content` only for hosts that cannot consume structured content and explicitly request `format=json`;
- protocol metadata containing safe binding/catalog/protocol IDs, never semantic fields duplicated from `ApplicationResponse`;
- `isError` plus a typed safe problem mapping for application errors.

The compatibility layer must not double-encode JSON or make host support change semantic fields. Conformance decodes both host paths back to the same canonical fixture.

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
tracedecay automation  schedules, runs, outcomes, authority, health
tracedecay config      complete plan-20 settings tree
tracedecay project     registry, repositories, worktrees, enrollment, sync
tracedecay system      status, doctor, daemon, update, migration, accounting
tracedecay lab         query/search/hint/coordination/config/privacy replay
tracedecay invoke      generated direct binding bridge
tracedecay help        catalog-backed discovery
```

The `system` group also exposes the generated `api token create`, `api token list`, and `api token revoke` bindings for plan 17's scoped, TTL-bound, revocable local API tokens, mapping plan 09's token-management command use cases; plan 10's per-launch bearer remains only the bootstrap credential that mints the initial admin token.

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

### 9.3 Hints and skills

Policy/hook hints reference stable intent/capability/binding IDs and a generated summary. They never paste the full tool list. A hint may recommend the cheapest applicable binding, the exact scope it would use, and one command. Hint analytics record offered/acted/missed/corrected/suppressed outcomes by IDs, not free-text matching.

Managed skills declare versioned capability requirements and allowed effects. Installation validates binding availability/catalog digest. Skills cannot resurrect removed aliases, invoke curation item approval paths, or teach surface-local scope/output behavior.

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
pub enum ExecutionModeV2 {
    ReadOnly,
    DirectCommit,
    ConfirmedDestructive,
    AutonomousPolicyEffect,
    ResumableWorkflow,
    InternalHostLifecycle,
}
```

Each binding exposes grants, idempotency, expected-version policy, audit schema, progress/receipt shape, cancellation boundary, and recovery. CLI/MCP annotations are generated from this enum. Read-only hosts cannot obtain mutation bindings merely through a name alias.

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

### 11.4 Confirmed destructive operations

Wipe, source cleanup, protected-data retirement, unsafe migration cutover, or external side effects can require an explicit confirmation token and current-version revalidation. Their names and receipts must describe the real effect. “Apply” and “rollback” are not generic framework verbs; use a domain command such as `migration cutover`, `migration recover`, or `project retire` where that is the actual operation. Edit use cases such as `usecase.code.move-symbol` follow the same rule: they declare one binding whose semantic request carries a typed preview input under their declared execution mode, so plan 09's preview/commit wording maps onto that single binding rather than separate generic `preview`/`apply` verbs.

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

Useful partial results return 0 with `coverage.complete=false` unless the caller requests `--require-complete`, in which case the same response is written and exit 5 communicates the unmet contract. Empty complete and empty incomplete remain different in output.

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

All bounded collections use the one page envelope defined in plan 17's contract IR; plan 10's `Page<T>` and this plan's `CursorPage<T>` are that same type, not variants:

```rust
pub struct CursorPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<OpaqueCursor>,
    pub truncation: Option<TruncationReason>,
    pub count_semantics: CountSemantics,
    pub ordering: OrderingReceipt,
}
```

The earlier draft's `returned` and `page_limits` fields fold into this shape without information loss: returned-row counts are part of `count_semantics`, and the applied default/hard page limits travel in the `truncation` receipt.

Opaque authenticated cursors bind the canonical request fingerprint, access digest, resolved scope, schema/ranking/index/catalog versions, frozen watermarks, expiry, ordering cutoff, and per-shard positions. CLI exposes `--cursor`; SDKs expose bounded pagers. `--all-pages` is allowed only with explicit maximum pages/items/bytes/deadline. No SQLite transaction spans client think time.

### 13.2 Output budgets

Pagination limits semantic row count. Presentation budgets limit human detail. Transport budgets limit encoded bytes/tokens. These are separate receipts. A renderer may reduce optional columns/detail according to a declared presentation ladder, but it cannot remove rows without the semantic page reporting it.

### 13.3 Retrieval anchors

When an individual eligible field or complete encoded response exceeds a transport cap:

- store the sanitized typed payload or eligible blob reference, not an unclassified rendered string;
- return an opaque `RetrievalAnchorV2` bound to privacy domain, principal/access digest, scope resolution, snapshot, format/schema, catalog digest, content digest, expiry, and retention policy;
- expose exact omitted fields/bytes and retrieve binding;
- authorize and revalidate on retrieval;
- regenerate presentation from the typed payload where possible;
- preserve handles across CLI/MCP/API when the same principal and scope permit;
- forbid a response handle as the only durable citation, saved-view, or export locator.

If storage is unavailable, return a typed `response_budget_exceeded` problem with a narrower request/cursor recommendation. Never emit `compacted_no_handle`, an invalid JSON prefix, or a false complete result.

### 13.4 Current regression cases

Fixture-lock:

- generic Markdown and JSON exceeding the current 15,000-character boundary;
- missing project root/handle cache;
- expired, missing, wrong-project, wrong-profile, wrong-access, and corrupt handles;
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
- Active state is computed once in the application response and reused everywhere.
- Status commands and MCP status tools consume the shared `SystemStatusSnapshot`; they do not aggregate different component meanings under local booleans.

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

Every binding declares a `SurfaceBudgetV1`; defaults are reviewed by use-case family rather than copied into handlers.

Targets on the reference machine:

- catalog exact binding lookup and generated dispatch: <=100 microseconds p95 excluding use-case execution;
- typed view to Markdown/terminal rendering for a default page: <=2 milliseconds p95 and <=2 MiB transient allocation;
- canonical JSON serialization for a default page: <=2 milliseconds p95;
- CLI parser/help startup after process launch: <=100 milliseconds p95 excluding daemon handshake;
- default MCP Markdown: <=4,000 estimated tokens soft budget; catalog-declared hard transport cap with cursor/retrieval recovery;
- default discovery result: <=1,000 tokens; one capability detail <=2,000 tokens;
- tool definition descriptions and examples: compact static metadata, with full docs retrieved explicitly rather than loaded into every host prompt;
- default collection pages: normally 20–50 items, hard cap declared per use case; no unbounded `limit`;
- NDJSON/SSE queues: bounded items/bytes and explicit gap/resync behavior;
- 1,000 repeated renders of the same typed fixture are byte-stable and leak no state;
- large result rendering scales linearly in returned rows/eligible bytes.

Benchmarks separately measure application execution, view construction, rendering, serialization, transport framing, truncation/anchor storage, and CLI/MCP overhead. Analytics record safe aggregate latency/bytes/tokens/format/truncation/cursor use by binding ID, never result text.

## 17. Compatibility, names, aliases, and deletion

### 17.1 Alias policy

- aliases are catalog rows with source name, canonical binding, exact semantic equivalence, introduced version, warning policy, cutoff, and docs replacement;
- an alias cannot change scope/default/effect/output or accept fields the canonical binding rejects;
- incompatible semantics get a new binding/use-case major, not an alias;
- old names may be searchable in help after cutoff but are not invokable;
- hidden provider commands are versioned internal bindings, not user aliases;
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
6. publish new binding plus warning-only exact aliases during the bounded window;
7. update installers/plugins/skills/docs/completion in one release;
8. reject stale protocol/catalog clients with one replacement/update action;
9. remove aliases and old dispatch from live surfaces at cutoff;
10. delete handler-local args, allowlists, renderers, prints/exits, and docs;
11. retain only frozen inventory/replay fixtures until the data rollback window ends;
12. publish a deletion receipt proving zero live references.

### 17.3 Mandatory deletions

After final cutover delete or reduce to generated adapters:

- hand-maintained MCP definition, format-capable, project-selector, profile-tool, first-touch, and dispatch lists;
- native command-local output tables/JSON branches/progress/exit logic;
- `generic_md` over arbitrary JSON and handler-specific format parsing;
- irreversible truncation and handler-local LCM compaction envelopes;
- transport-routing argument validation bypass lists;
- duplicated project label/active/missing-registry renderers;
- V1 curation approval/apply/reject/draft/install surfaces;
- active aliases beyond cutoff;
- live legacy CLI/MCP fallback paths.

## 18. Generated documentation, schemas, completion, and parity matrix

Generate:

- complete CLI reference including hidden/internal appendix and alias cutoffs;
- complete MCP reference with schemas, effects, auth, scope, formats, errors, limits, availability, and examples;
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

### 19.2 Every-tool format conformance

For every readable MCP tool:

1. invoke with format omitted and assert valid compact Markdown;
2. invoke `format=json` and decode the canonical typed schema;
3. compare item identity/order/count/coverage/freshness/redaction/limits between modes;
4. assert the definition advertises exactly the implemented formats;
5. assert missing/empty/partial/error/large-result fixtures;
6. assert no raw JSON dump, dropped field, false empty state, double encoding, or irreversible truncation.

Give dedicated regression fixtures to `dsm`, `files`, `sessions_for`, `type_hierarchy`, and `workflows`; the current schema/render mismatch must fail before implementation. Keep the `unsafe_patterns` wrong-renderer/false-empty case as a permanent typed-view test.

For every mutation/internal tool, assert effect class, auth, idempotency/version, receipt, stdout/MCP problem, safe failure, and absence of unsupported human/JSON modes.

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

### 19.4 Cross-transport semantic parity

Run one canonical fixture per use case through in-process application, native CLI JSON, generic CLI invoke JSON, MCP JSON, HTTP, Rust SDK, TypeScript SDK, Python sync/async SDK, and dashboard client where applicable. Compare after removing transport-only request/framing/timing fields:

- scope resolution and active label;
- rows/edges/facets/order/scores/count semantics;
- coverage/freshness/watermarks/redactions/retention/limits;
- cursor/restart/retrieval anchors;
- error code/retry/candidates/current version/operation;
- command effect/idempotency/audit/recovery;
- autonomous curation policy/run/outcome views;
- configuration effective values/provenance/impact;
- Git local/live/joined truth.

### 19.5 Security, fuzz, and accessibility

- plan-18 positive/negative secret corpus through every format and error path;
- Markdown/HTML/link/ANSI/OSC/control/Unicode/bidi/zero-width/CSV-formula/JSON-line injection;
- repeated basename and credential-bearing remote URL fixtures;
- malicious catalog description, alias, field label, path, provider error, and retry text;
- response handle guessing, expiry, scope/auth replay, corruption, and path traversal;
- terminal widths 40/80/120/200, screen-reader/no-color/high-contrast copy behavior, and deterministic tables;
- property tests proving renderer never changes semantic row membership.

### 19.6 Scale and fault matrix

- full catalog with thousands of extension bindings and fast help/search;
- thousands of projects/worktrees, concurrent agent readers/writers, partial shards, locked registry, daemon restart, stale protocol, disk full, handle-store failure, and cancellation;
- slow NDJSON/SSE consumers, bounded backpressure, gap/resync, and exact final coverage;
- renderer panic/serialization failure converts to safe invariant problem without partial stdout;
- identity-cutover conflict returns the same candidates/remediation through CLI/MCP/HTTP.

## 20. Implementation slices inside the existing master program

These are sub-slices of existing PRs, not a separate architecture track.

### PR 1/3 companion — frozen surface/output audit

- Generate the recursive CLI and full MCP source/runtime inventories.
- Record every current inconsistency from Section 2 and every row from Sections 3–4.
- Add source/runtime/release drift and TraceDecay identity-conflict fixtures.

### PR 4/9 companion — output-safe domain and store contracts

- Add presentation/retrieval IDs, count/order/page/anchor/output budget contracts where not already owned.
- Persist sanitized typed retrieval anchors and expiry/audit metadata without adding renderer behavior to stores.

### PR 22A companion — binding and presentation specs

- Extend the capability catalog with formats, presentation IDs, effect modes, exit classes, stream/export support, cursor/anchor, and budgets.
- Generate CLI/MCP schemas/help/docs/parity matrix and reject all duplicate allowlists.

### PR 24A companion — sealed typed application views

- Replace raw result maps with domain-clustered transport-eligible views.
- Standardize coverage, empty/missing/partial states, labels, errors, and operations.
- Carve autonomous curation and direct configuration out of any generic preview/apply command abstraction.

### PR 24E1–24E8 companion — pure presentation plus generated adapters

- Land `tracedecay-presentation` boundary or equivalent sealed root module.
- Cut CLI and MCP domains over one at a time with semantic/presentation differential tests.
- Normalize stdout/stderr/exits, format switches, scope builders, help, cursors, and retrieval anchors.

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

- Delete hand-maintained definitions, routing lists, format branches, raw renderers, local exits, expired aliases, and curation proposal surfaces.
- Require zero uncataloged command/tool, zero semantic duplicate, zero raw-Value renderer, zero irreversible truncation, and zero V1 live fallback.

## 21. Verification commands and artifacts

Implementation verification from the repository root includes:

```bash
cargo test -p tracedecay-tool-catalog complete_inventory transport_parity
cargo test -p tracedecay-application surface_views
cargo test -p tracedecay-presentation
cargo test -p tracedecay-api cli_mcp_http_sdk_parity
cargo nextest run --workspace --no-fail-fast
pnpm --dir dashboard test -- settings command-palette
pnpm --dir dashboard exec playwright test settings output-inspector autonomy
gitleaks git --redact --no-banner
gitleaks dir generated docs dashboard packages python tests --redact --max-archive-depth 2
```

Required artifacts:

- source/runtime CLI and MCP inventory manifests with digests;
- full disposition and parity matrices;
- generated help/schema/docs/completion hashes;
- semantic and presentation golden fixtures;
- current/V2 differential report by binding/use case;
- performance/token/byte/latency benchmark report;
- secret/injection/accessibility/fault receipts;
- alias cutoff and stale-client conformance report;
- final deletion receipt.

Plan-file checks before handoff:

```bash
test "$(rg -c '^```' docs/superpowers/plans/tracedecay-v2/21-cli-mcp-tool-surface-and-output-unification.md)" -ge 2
rg -n 'TB[D]|TO[D]O|FIXM[E]|PLACEHOLDE[R]|00x[x]|implement late[r]|fill i[n]' docs/superpowers/plans/tracedecay-v2/21-cli-mcp-tool-surface-and-output-unification.md
gitleaks dir docs/superpowers/plans/tracedecay-v2 --redact --no-banner
```

The fence test is supplemented by a parser that requires an even fence count and validates local Markdown links.

## 22. Definition of done

- [ ] Every current visible/hidden/aliased CLI path and all 104 source MCP definitions have one reviewed use-case/binding/lifecycle disposition.
- [ ] Source, runtime, installed plugin, generated docs, and tests agree on advertised/available tool sets; host-conditional absence has a typed reason.
- [ ] One catalog generates names, descriptions, params/defaults, scope, effects, auth, formats, help, docs, completion, and parity fixtures.
- [ ] One application use case executes each semantic operation; CLI/MCP contain no store/query/policy behavior.
- [ ] Machine JSON is canonical typed semantic data across CLI/MCP/HTTP/SDK and is never a double-encoded transport wrapper.
- [ ] MCP defaults to compact Markdown; CLI defaults to deterministic human output; all explicit formats are schema-advertised and tested.
- [ ] No public renderer accepts raw JSON/string payloads or can silently drop rows/fields/coverage.
- [ ] Every collection has deterministic order, authenticated cursor, caps, and resumable SDK/CLI behavior.
- [ ] Every transport-size truncation is explicit and recoverable, or returns a safe budget error; no `compacted_no_handle` remains.
- [ ] Missing registry, active marker, repeated basename, partial/stale/locked/redacted, and empty states have stable typed shapes.
- [ ] stdout/stderr and exit codes are stable; command modules do not call `process::exit` or print machine-breaking prose.
- [ ] All project/repository/worktree/ref/profile/provider selection uses unchanged `ScopeSelectorV2`; ambiguity never first-matches.
- [ ] Redactor and every non-secret configuration key are fully visible/navigable in Brain Settings and generated CLI/MCP/API/SDK surfaces, subject to the non-disableable floor.
- [ ] Configuration edits validate and save directly; no routine preview/apply/rollback ceremony exists.
- [ ] Curation/self-improvement is fully autonomous; no per-item preview/approve/reject/apply/install/rollback binding or UI control exists.
- [ ] Destructive non-curation operations retain explicit confirmation, audit, idempotency, and recovery where required.
- [ ] Help, hints, and skills route by stable IDs and never teach stale aliases, duplicated scope logic, or full-catalog spam.
- [ ] Markdown, terminal, table, JSON, NDJSON, SSE, exports, errors, docs, and fixtures pass privacy/redaction/injection gates.
- [ ] Performance, token, deterministic generation, cross-transport parity, scale, fault, and accessibility gates pass.
- [ ] Hand-maintained definition/format/scope/routing lists, raw renderers, local output branches, proposal queues, expired aliases, and V1 live fallbacks are deleted.
- [ ] The final plan set tells one flow: cataloged intent -> generated binding -> shared scope/auth -> application use case -> typed view -> safe renderer/serializer -> cursor/coverage/anchor -> audit/analytics, with no parallel semantic path.
