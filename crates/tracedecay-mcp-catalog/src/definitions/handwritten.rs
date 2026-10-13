//! The ordered table of MCP tool definitions written by hand.
//!
//! Each row owns its transport name, title, description, and effect. The
//! input schema is the canonical application request schema unless the row
//! names a handwritten schema function. Row order is `tools/list` order.

use serde_json::Value;

use super::{CONTEXT_DESCRIPTION, RESPONSE_RETRIEVE_TOOL, edit, lcm, multi_root, session};
use crate::McpCatalogError;

pub(super) struct HandwrittenTool {
    pub(super) name: &'static str,
    pub(super) title: &'static str,
    pub(super) description: &'static str,
    pub(super) read_only: bool,
    pub(super) schema: ToolSchema,
}

pub(super) enum ToolSchema {
    /// The canonical executable request schema of the application operation
    /// the tool name spells.
    Application,
    Handwritten(fn() -> Value),
    Contract(fn() -> Result<Value, McpCatalogError>),
}

const fn read(
    name: &'static str,
    title: &'static str,
    description: &'static str,
) -> HandwrittenTool {
    HandwrittenTool {
        name,
        title,
        description,
        read_only: true,
        schema: ToolSchema::Application,
    }
}

const fn write(
    name: &'static str,
    title: &'static str,
    description: &'static str,
) -> HandwrittenTool {
    HandwrittenTool {
        read_only: false,
        ..read(name, title, description)
    }
}

impl HandwrittenTool {
    const fn schema(self, schema: fn() -> Value) -> Self {
        Self {
            schema: ToolSchema::Handwritten(schema),
            ..self
        }
    }

    const fn contract(self, schema: fn() -> Result<Value, McpCatalogError>) -> Self {
        Self {
            schema: ToolSchema::Contract(schema),
            ..self
        }
    }
}

pub(super) const HANDWRITTEN_TOOLS: &[HandwrittenTool] = &[
    read(
        "tracedecay_search",
        "Search Symbols",
        "Exact and lexical code search over the active project's code graph: find symbols (functions, structs, traits, etc.) by name, identifier fragment, signature, path, or phrase through ranked exact and lexical routes. Every response opens with a `freshness: fresh | possibly_stale` line, so no status preflight is needed. Pass known identifiers as `lexical_anchors` (each an extra ranked route) and set `prefer_symbol` to add a symbol-name route for identifier-shaped query words.",
    ),
    read(
        "tracedecay_grep",
        "Grep Content",
        "grep, ripgrep, rg, text search, find string. Literal/regex content search over UTF-8 text sources in the project working tree (respects .gitignore; binary and non-UTF-8 files are outside the search scope), graph-enriched: each hit resolves the enclosing symbol so the natural next call is tracedecay_source_body with its node_id. Bounded file or line omissions and unavailable source candidates are reported as partial coverage. Routing: use this for literal/regex content search (string literals, config keys, error messages); for symbol names use tracedecay_search; for concepts use tracedecay_context. Defaults to the active project; pass project_selector.project_id only when intentionally searching another registered project.",
    ),
    read(
        "tracedecay_ast_grep_search",
        "AST Structural Search",
        "Structural (AST) code search: find call shapes, argument orders, and other \
         syntax-tree patterns that a text regex cannot express. Uses ast-grep's SGPattern \
         syntax (metavariables: `$X` one node, `$$$` many). Runs IN-PROCESS over the project \
         working tree using the bundled tree-sitter grammars, no external ast-grep binary, no \
         gating. Each hit resolves its enclosing symbol, so the natural next call is \
         tracedecay_source_body with that symbol's node_id. Routing: use this when the pattern is structural (e.g. `foo($$$)`, \
         `if ($C) { $$$ }`); for a literal/regex string use tracedecay_grep; for a symbol name \
         use tracedecay_search. To rewrite a structural match, pair with tracedecay_ast_grep_rewrite.",
    ),
    read(
        RESPONSE_RETRIEVE_TOOL,
        "Retrieve Truncated Response",
        "Use `tracedecay_retrieve` with required argument `handle` to read one bounded page of a truncated MCP response's locally cached text. Pass offset and max_chars for the omitted span you need. Do not walk next_offset until has_more is false or concatenate pages back into the conversation: the stored body already exceeded the response budget. max_chars is clamped to the response-frame budget. This does not re-run the source tool or read a file/session/node again; handles are scoped to the active project store, expire automatically, and never reference remote storage. If the original truncated response used project_selector.project_id, pass the same selector here. Only call it when the missing details are needed to answer the user's request.",
    ),
    read(
        "tracedecay_context",
        "Task Context",
        CONTEXT_DESCRIPTION,
    ),
    read(
        "tracedecay_impact",
        "Impact Radius",
        "Compute the impact radius of a node: all symbols that directly or indirectly depend on it.",
    ),
    read(
        "tracedecay_node",
        "Node Details",
        "Retrieve detailed information about a single node by its ID.",
    ),
    read(
        "tracedecay_status",
        "Graph Status",
        "Return a compact summary of the code graph (counts and freshness). Full branch diagnostics are opt-in.",
    ),
    read(
        "tracedecay_active_project",
        "Active Project",
        "Return the resolved active project context for this MCP session, including project ID, project root, scope prefix, branch identity, and the active project store paths. Use this instead of guessing from repo-local marker files or hardcoded DB paths.",
    ),
    read(
        "tracedecay_project_list",
        "Project List",
        "List projects from the profile/global registry without opening or mutating their stores. Results are bounded and include only registry metadata. Output is grouped into a `project_tree` by repository alongside a `summary`, and the calling project is marked with `is_active` when it is registered.",
    ),
    read(
        "tracedecay_project_search",
        "Project Search",
        "Search registered projects by project id, root path, aliases, or default branch. This is read-only and bounded; output omits credential-bearing remotes. Output is grouped into a `project_tree` by repository alongside a `summary`, and the calling project is marked with `is_active` when it is registered.",
    ),
    read(
        "tracedecay_project_context",
        "Project Context",
        "Return registry context for one project: project metadata, aliases, store instances, graph scopes, and artifacts. Defaults to the active project alias when neither project_selector nor path is provided.",
    ),
    read(
        "tracedecay_files",
        "File List",
        "List indexed project files. Use to explore file structure without reading file contents.",
    ),
    read(
        "tracedecay_multi_root_scope_set_read",
        "Read a saved multi-root scope set",
        "Read one authorized multi-root scope set through the daemon-owned route. The daemon conceals absent and unauthorized records identically.",
    )
    .schema(multi_root::multi_root_scope_set_read_schema),
    write(
        "tracedecay_multi_root_scope_set_compare_and_swap",
        "Save a multi-root scope set",
        "Create or replace one exact registered-root scope set through the daemon's coordinated compare-and-swap and recovery authority.",
    )
    .schema(multi_root::multi_root_scope_set_compare_and_swap_schema),
    read(
        "tracedecay_multi_root_execute",
        "Execute a frozen multi-root query",
        "Execute one closed query family against an exact saved scope-set revision. Continuations are daemon-authenticated and retain frozen root generations.",
    )
    .contract(multi_root::multi_root_execute_schema),
    read(
        "tracedecay_remote_status",
        "Read Remote Brain status",
        "Read the Remote Brain operational plane: listener, enrollment, spool, replay coverage, and failover/recovery state.",
    ),
    read(
        "tracedecay_affected",
        "Affected Tests",
        "Which tests to run, run affected tests, tests impacted by a change. Find test files affected by changed source files via dependency graph traversal.",
    ),
    read(
        "tracedecay_dead_code",
        "Dead Code",
        "Find symbols with no incoming edges (potentially unreachable code). \
         Always excludes `main` and `test*` functions. By default also excludes \
         `pub` items (they may be referenced outside the indexed scope), pass \
         `include_public: true` to audit pub items with zero indexed callers, \
         which is what you want for workspace-internal cleanup. Pass `path` to \
         report only one directory prefix, which keeps a fixture or benchmark \
         corpus from consuming the whole page; the prefix is applied before \
         `limit`, and references from outside it still count as callers.",
    ),
    read(
        "tracedecay_diff_context",
        "Diff Context",
        "Given changed file paths, return semantic context: which symbols were modified, what depends on them, and affected tests.",
    ),
    read(
        "tracedecay_circular",
        "Circular Deps",
        "Detect circular dependencies between files in the code graph.",
    ),
    read(
        "tracedecay_hotspots",
        "Hotspots",
        "Find symbols with the highest connectivity, weighted by how often their file changed in git over the last 90 days. A file with no commits in that window has churn 0, which is a measurement. When git history cannot be read, churn is omitted and the ranking is connectivity alone.",
    ),
    read(
        "tracedecay_similar",
        "Shared Implementations",
        "Find token-verified implementations that share exact or rename-normalized source with one symbol occurrence or source range. The operation accepts no free-text query and reports bounded coverage for each digest family.",
    ),
    read(
        "tracedecay_redundancy",
        "Repository Shared Implementations",
        "Report token-verified exact and rename-normalized implementation families in one authorized repository. Results are ranked by reviewable source bytes and carry bounded family and member coverage.",
    ),
    read(
        "tracedecay_rename_preview",
        "Rename Preview",
        "READ-ONLY preview of what a rename would touch. Given a symbol node and \
         an optional `new_name`, returns the declaration site plus every graph \
         reference site (from call/use/etc. edges), each with its current-text \
         line snippet, and a per-file count of literal textual occurrences of \
         the name that are NOT graph references ('text-only matches, review \
         manually'). It does NOT edit anything and does NOT rewrite occurrences. \
         Pass the reported node identity to `tracedecay_rename_symbol` to \
         apply the rename. Graph call-edge coverage improves as the resolver \
         does; text-only counts catch what the graph misses (comments, \
         strings, dynamic dispatch, unresolved refs).",
    ),
    read(
        "tracedecay_unmounted_files",
        "Unmounted Files",
        "Find source files present on disk that nothing reaches, files the code graph indexes as \
         healthy symbols while no compiler, bundler, or test runner ever loads them. Reports one \
         section per ecosystem in a typed `ecosystems` field, each carrying its own verdict, \
         counts, and blind-spot list. RUST (cargo): walks every package found by manifest sweep \
, workspace members, path dependencies, nested independent workspaces, from its own \
         roots (src/lib.rs, src/main.rs, src/bin/*, every tests/*.rs and tests/<name>/main.rs, \
         benches, examples, build.rs), follows `mod name;`, `#[path = \"...\"]`, \
         `#[cfg_attr(..., path = \"...\")]` and `include!(\"literal.rs\")`, and diffs the \
         reachable set against the .rs files under that package's source directories; a finding \
         means the compiler never parses the file, and names the nearest mounted parent module \
         plus the exact `mod` line to add. TYPESCRIPT/JAVASCRIPT (npm, incl. pnpm/npm/yarn \
         workspaces, discovered per package.json rather than per glob): walks static `import`, \
         `require`, `export ... from`, and literal dynamic `import()` from declared entry points \
, package.json main/module/browser/types/bin/exports and the file paths named in \
         `scripts`, string literals in root `*.config.*` files (how an rsbuild/vite entry or a \
         vitest setup file is found without executing the config), tsconfig `files` and `paths` \
         aliases, plus conventional roots (tests, stories, `*.d.ts`, `src/index.*`, Next.js \
         app/pages route files); a finding means only that no static import path reaches the file \
, `tsc` may still type-check it via a tsconfig `include`, so no repair line is invented. \
         Predicates are never evaluated: a cfg-gated module or a conditionally-aliased file \
         counts as mounted. Languages with no reachability model here (Python, Go, Java, C/C++, \
         Ruby, and the rest) are reported with status `unsupported` and a file count, never \
         silently omitted. Per-ecosystem blind spots ride in the response; the standing ones are \
         macro-expanded `mod`s and computed `include!` paths for Rust, and non-literal dynamic \
         imports, non-tsconfig bundler aliases, glob imports, `.vue`/`.svelte`/`.astro` files, \
         and HTML/CSS-only references for TypeScript. Paths listed in the workspace's \
         `[workspace.metadata.cargo-shear] ignored-paths` are excluded.",
    ),
    read(
        "tracedecay_rank",
        "Rank",
        "Rank nodes by edge count for a given relationship type (calls, implements, extends, etc.).",
    ),
    read(
        "tracedecay_largest",
        "Largest Symbols",
        "Rank nodes by size (line count). Find the largest classes, longest methods, biggest enums, etc.",
    ),
    read(
        "tracedecay_coupling",
        "Coupling",
        "Rank files by coupling: fan_in (most depended on) or fan_out (most dependencies).",
    ),
    read(
        "tracedecay_inheritance_depth",
        "Inheritance Depth",
        "Find the deepest class/interface inheritance hierarchies by walking extends chains.",
    ),
    read(
        "tracedecay_distribution",
        "Distribution",
        "Show node kind distribution (classes, methods, fields, etc.) per file or directory.",
    ),
    read(
        "tracedecay_recursion",
        "Recursion",
        "Detect recursive and mutually-recursive call cycles in the call graph.",
    ),
    read(
        "tracedecay_complexity",
        "Complexity",
        "Rank functions/methods by composite complexity score (lines + fan-out + fan-in).",
    ),
    read(
        "tracedecay_doc_coverage",
        "Doc Coverage",
        "Find public symbols missing documentation (docstrings).",
    ),
    read(
        "tracedecay_god_class",
        "God Classes",
        "Find classes with the most members (methods + fields).",
    ),
    read(
        "tracedecay_changelog",
        "Changelog",
        "git log, git history, git blame, diff between refs. Generate a semantic diff/changelog between two git refs, categorizing symbols as added, removed, or modified.",
    ),
    read(
        "tracedecay_port_status",
        "Port Status",
        "Compare symbols between source and target directories to track porting progress.",
    ),
    read(
        "tracedecay_port_order",
        "Port Order",
        "Topological sort of symbols in a directory -- port leaves first, dependents after.",
    ),
    read(
        "tracedecay_commit_context",
        "Commit Context",
        "git diff, git status, git log style, staged changes for a commit message. Semantic summary of uncommitted changes for drafting a commit message. Returns changed symbols, file roles, and recent commit style.",
    ),
    read(
        "tracedecay_pr_context",
        "PR Context",
        "Semantic summary of changes between two git refs for drafting a pull request description.",
    ),
    read(
        "tracedecay_test_map",
        "Test Map",
        "Which tests cover this, run tests for a symbol, test coverage. Map source symbols to their test functions by walking the call graph up to depth 3. A listed test may be a direct caller or a transitive caller reached through up to two intermediate functions; coverage here is static attribution (the symbol is reachable from a test), not executed line/branch coverage. Pair with tracedecay_test_risk to see the direct-vs-closure attribution_method distinction per symbol. Pass `file` or `node_id`.",
    ),
    read(
        "tracedecay_branch_search",
        "Cross-Branch Search",
        "Search the immutable code-index generation sealed for a local branch's exact current commit.",
    ),
    read(
        "tracedecay_branch_diff",
        "Branch Diff",
        "Compare immutable code-index generations sealed for two local branches' exact commits.",
    ),
    read(
        "tracedecay_branch_list",
        "List Tracked Branches",
        "List a bounded snapshot of exact local branch refs and their current commit identities.",
    ),
    write(
        "tracedecay_str_replace",
        "Edit File",
        "Replace a unique string in a file with new content. Fails if the old string is not found or matches more than once. This is the safest edit primitive. Use this instead of sed/awk.",
    )
    .schema(edit::str_replace_schema),
    write(
        "tracedecay_multi_str_replace",
        "Multi-Edit File",
        "Apply multiple string replacements atomically in a single file. All replacements must match exactly once. If any replacement fails (0 or >1 matches), the entire operation is aborted and no changes are made.",
    )
    .schema(edit::multi_str_replace_schema),
    write(
        "tracedecay_insert_at",
        "Insert Into File",
        "Insert content before or after a unique anchor in a file. The anchor can be a unique string or a 1-indexed line number. Fails if the anchor matches more than one line.",
    )
    .schema(edit::insert_at_schema),
    write(
        "tracedecay_ast_grep_rewrite",
        "AST Structural Rewrite",
        "Rewrite every structural match of an ast-grep pattern in one file. The pattern and rewrite use ast-grep's SGPattern syntax ($A captures one node, $$$ a run of nodes); matches in comments and strings are left alone. The language is inferred from the file extension.",
    )
    .schema(edit::ast_grep_rewrite_schema),
    read(
        "tracedecay_gini",
        "Gini Inequality",
        "Compute inequality (Gini coefficient) for any metric across files or symbols. Detects god files and uneven complexity distribution.",
    ),
    read(
        "tracedecay_dependency_depth",
        "Dependency Depth",
        "Show the longest file-level dependency chains. Files at the end of long chains are fragile to upstream changes.",
    ),
    read(
        "tracedecay_health",
        "Health Score",
        "Health panel, worst measured dimension first in `panel`. Fix the lowest-scoring dimension it measured. `panel` then lists `no evidence:` dimensions with the reason, such as no dependency edges or fewer than 2 files; their scores measure nothing in this scope. quality_signal is the geometric mean of acyclicity, depth, equality, redundancy, and modularity (0-10000), reduced by up to 2% for skip-test-coverage overuse. It is a breakdown, not a target. A zero in one dimension zeroes it. No-evidence dimensions enter it at their formula value, so an empty scope reads 10000 and one file reads 0. Pass details=true for the per-dimension scores.",
    ),
    read(
        "tracedecay_runtime",
        "Runtime Snapshot",
        "Capture a process + database telemetry snapshot for the running tracedecay MCP server: PID, resident memory, virtual size, sustained CPU% (sampled over ~200ms), thread count, system memory, DB / WAL / SHM file sizes, journal mode, and the DB-to-source byte ratio. Use this when triaging unexpected CPU or RAM consumption (issue #80). Set authority_audit=true only for exhaustive Doctor-style observation-authority validation. Single call, output is a JSON object.",
    ),
    read(
        "tracedecay_dsm",
        "Design Structure Matrix",
        "Get the Design Structure Matrix: file dependency summary showing directory clusters, edge density, and an optional file matrix. It does not assign architecture layers.",
    ),
    read(
        "tracedecay_test_risk",
        "Test Risk",
        "Find high-risk source symbols with weak or no static test attribution. Reports both direct test-call coverage and conservative depth-3 closure attribution so integration-heavy repos do not look artificially uncovered. Each risk item carries an attribution_method (direct_unit vs closure); coverage_pct is a static attribution lower bound, not executed line/branch coverage. Answers: where should the next test go, and what only has broad behavioral evidence today?",
    ),
    read(
        "tracedecay_todos",
        "TODOs and FIXMEs",
        "Find TODO, FIXME, XXX, HACK, WIP, NOTE, and unimplemented markers across the project. \
         Each result includes the marker kind, file, line, the comment text, and the enclosing \
         symbol name (function/method) for quick orientation.",
    ),
    read(
        "tracedecay_by_qualified_name",
        "Lookup by qualified name",
        "List the locations of up to 1000 symbols whose qualified name equals the input exactly and case-sensitively, covering overloads, generics, and separate impl blocks. \
         Use find_exact_symbol for a bare name, qualified_name for full symbol records, or signature for the matches' signatures. \
         Useful for cross-run lookups where the content-hash node ID has changed.",
    ),
    read(
        "tracedecay_signature",
        "Signature",
        "Return signatures without bodies for the symbol at `node_id`, or for every symbol whose qualified name equals `qualified_name` exactly; `node_id` wins when both are set. \
         Each row carries visibility, the signature string (generics, params, return type, \
         where clauses), docstring, async flag, kind, and location. Use this \
         instead of reading source files when you only need the public-API \
         surface of a function, method, or type; use find_exact_symbol first when you only know a bare name.",
    ),
    read(
        "tracedecay_diagnose",
        "Diagnose Cargo Output",
        "Parse raw `cargo check` / `cargo clippy` stderr text and map each \
         diagnostic to the smallest containing graph node, with callers \
         pre-attached so you can see what the failing code is reachable \
         from. Diagnostics without a `--> file:line:col` span are dropped. \
         Pass the full stderr capture; you do not need to pre-filter.",
    ),
    read(
        "tracedecay_derives",
        "Derives on Type",
        "List the exact `#[derive(...)]` macro names attached to a type. Each \
         name carries `syntax_exact` evidence. Generated trait implementations \
         and methods are reported unavailable because macro expansion is not \
         retained in the verified graph.",
    ),
    write(
        "tracedecay_run_affected_tests",
        "Run Affected Tests",
        "Run `cargo test` for tests that cover the symbols in the explicit \
         `changed_paths` manifest. Closes the loop opened by \
         `tracedecay_test_map` / `tracedecay_test_risk`, emits pass/fail per \
         test alongside the source nodes each test covers. Output is the \
         libtest summary parsed into JSON.",
    ),
    write(
        "tracedecay_fact_store_add",
        "Fact Store Add",
        "Add one holographic memory fact. The result includes a write-time diff report for near duplicates, possible conflicts, and rejected secret-like content. Calibrate trust to the evidence instead of defaulting high.",
    ),
    read(
        "tracedecay_fact_store_search",
        "Fact Store Search",
        "Rank durable memory facts against free text by text match, holographic similarity, trust, recency, and retrieval count, with a `why` for each score. \
         Use fact_store_probe or fact_store_reason when you know the exact entity names.",
    ),
    read(
        "tracedecay_fact_store_probe",
        "Fact Store Probe",
        "List memory facts whose entities include one exact entity (case-insensitive), ranked by trust. \
         Use fact_store_reason for facts naming several entities together, or fact_store_related for that entity's neighbors.",
    ),
    read(
        "tracedecay_fact_store_related",
        "Fact Store Related",
        "List memory facts about an entity's neighbors: facts naming any entity that shares a fact with it, expanded through the memory graph and ranked by trust. \
         It returns facts, not entities; use fact_store_probe for facts naming the entity itself.",
    ),
    read(
        "tracedecay_fact_store_reason",
        "Fact Store Reason",
        "List memory facts whose entities include every entity in `entities` (all must match, case-insensitive), ranked by trust. \
         With one entity it matches fact_store_probe; use fact_store_related to widen to neighboring entities.",
    ),
    read(
        "tracedecay_fact_store_contradict",
        "Fact Store Contradict",
        "Find potentially contradictory holographic memory facts above an optional threshold.",
    ),
    read(
        "tracedecay_fact_store_get",
        "Fact Store Get",
        "Get one holographic memory fact, including trust history explaining score changes.",
    ),
    write(
        "tracedecay_fact_store_update",
        "Fact Store Update",
        "Update one existing holographic memory fact without changing its identity.",
    ),
    write(
        "tracedecay_fact_store_remove",
        "Fact Store Remove",
        "Remove one holographic memory fact by exact fact id. An unknown fact id is refused as `not_found_or_not_authorized`, like `fact_store_get`.",
    ),
    write(
        "tracedecay_fact_store_supersede",
        "Fact Store Supersede",
        "Mark one holographic memory fact as superseded by another fact id. The old fact leaves default list/search/probe results but stays readable by id through its history; payload and trust are untouched. Use this when a newer fact corrects an older one instead of removing the old one. An unknown fact id is refused as `not_found_or_not_authorized`, like `fact_store_get`.",
    ),
    read(
        "tracedecay_fact_store_list",
        "Fact Store List",
        "List holographic memory facts with optional category, trust, and project selectors.",
    ),
    write(
        "tracedecay_fact_feedback",
        "Fact Feedback",
        "Record whether an active-project memory fact materially helped or misled the current work, and adjust its trust score.",
    ),
    read(
        "tracedecay_memory_status",
        "Memory Status",
        "Inspect canonical memory state: return the owner, fact/entity counts, vector algebra with its per-vector superposition estimate (not a fact limit), trust distribution, below-threshold facts, feedback totals, and retrieval funnel. Defaults to the active project; pass an exact project_selector only when intentionally checking another registered project. Human/operator equivalent: `tracedecay memory status`.",
    ),
    write(
        "tracedecay_fact_store_curate",
        "Fact Store Curate",
        "Start the daemon-owned automatic Memory Curator and return its run receipt (`run_id`, `state: started`) at admission. Callers may bound review size and confidence only; TraceDecay derives the run, operations, validation, policy, and apply authority. The run settles on the daemon; read its terminal with tracedecay_automation_run_view.",
    ),
    read(
        "tracedecay_automation_run_list",
        "Automation Run List",
        "List the newest durable automation run ledger records for the active project without triggering or mutating automation. Results are newest-first, deduplicated by run id, bounded to 200 records, and report whether the returned page is complete or contains malformed ledger rows.",
    ),
    read(
        "tracedecay_automation_run_view",
        "Automation Run View",
        "Read one exact durable automation run ledger record from the active project by run id without triggering or mutating automation. A missing run returns a typed not-found error and does not enumerate other run ids.",
    ),
    read(
        "tracedecay_automation_run_artifact_view",
        "Automation Run Artifact View",
        "Read and hash-verify one durable automation run artifact payload from the active project's dashboard sidecar. Returns the run id, artifact metadata, and JSON payload without mutating automation state. Human/operator equivalents: `tracedecay automation runs artifact <run_id> <kind> --json` and `GET /api/automation/runs/{run_id}/artifacts/{kind}`.",
    ),
    read(
        "tracedecay_skill_list",
        "Skill List",
        "List agent-managed skills from the active TraceDecay profile. Returns metadata, lifecycle state, support-file paths, usage summary, stale/archive and improvement recommendation evidence, and optional body text without mutating the skill store.",
    ),
    read(
        "tracedecay_skill_view",
        "Skill View",
        "Read one agent-managed skill package from the active TraceDecay profile. Returns metadata, body text, usage summary, and support-file path summaries. Support-file bytes are omitted unless include_support_files is true.",
    ),
    read(
        "tracedecay_hermes_skill_bridge",
        "Hermes Skill Inventory",
        "Read skills, pending approvals, usage telemetry, and archive counts owned by the standard ~/.hermes user install. Read-only; alternate Hermes roots and TraceDecay storage selectors are not supported.",
    ),
    write(
        "tracedecay_dashboard",
        "Dashboard",
        "Start (or manage) the tracedecay dashboard server for the current project as a background task inside the MCP server. Returns the listening URL. Idempotent: if already running, returns the existing URL. Pass action:\"stop\" to shut down a running instance. MCP dashboard binds are loopback-only: optional host must be 127.0.0.1, localhost, or ::1. Port is optional.",
    ),
    read(
        "tracedecay_analytics",
        "Usage Analytics",
        "Read-only adoption/telemetry rollup over the durable analytics_events table, the memory-fact funnel, and the automation run ledger. Answers 'what did the agent actually do' without querying .tracedecay databases directly: per-tool call/error counts grouped into tiers (navigation, analysis, session, memory, edit, admin), top-N tools by call volume, zero-call defined tools, hint emitted/followed/ignored/suppressed counts by category, the fact-store funnel (facts, retrievals, rated, helpful/unhelpful), and automation run outcomes (succeeded/failed/skipped) per job from the run ledger. Defaults to the active project over the last 14 days; pass scope:\"all\" for every registered project's analytics_events, or a project selector to inspect another registered project (fact/automation sections always report the resolved single project even in scope:\"all\").",
    ),
    write(
        "tracedecay_session_refresh_begin",
        "Session Refresh Begin",
        "Begin or join one daemon-owned durable session-temporal refresh. The operation is idempotent: repeating it joins the running refresh instead of starting a second one, and the typed started or joined outcome reports what occurred. Returns an opaque handle for tracedecay_session_refresh_status and tracedecay_session_refresh_cancel. Supply the public session, source, target, and scope kind; the daemon binds project, profile, Git-route, store, and root identity from the mounted authority. scope.kind=profile uses the authenticated profile session authority and never redirects through the active project. Unavailable authority fails closed without opening stores or ingesting transcripts.",
    ),
    read(
        "tracedecay_session_refresh_status",
        "Session Refresh Status",
        "Inspect one daemon-owned durable session-temporal refresh using the opaque handle returned by tracedecay_session_refresh_begin, with the same public scope, session, source, and target selectors. Mounted identity remains daemon-owned. Read-only: returns progress or the terminal receipt and never begins, resumes, or cancels a refresh.",
    ),
    write(
        "tracedecay_session_refresh_cancel",
        "Session Refresh Cancel",
        "Request durable cancellation of one daemon-owned session-temporal refresh using the opaque handle returned by tracedecay_session_refresh_begin, with the same public scope, session, source, and target selectors. Mounted identity remains daemon-owned. Success is receipt-backed; request abort or deadline outcomes never imply durable cancellation.",
    ),
    read(
        "tracedecay_message_search",
        "Message Search",
        "Read session-temporal message evidence from one authorized project or profile root. This tool never ingests or refreshes provider history. Omitted require_fresh is false; explicit require_fresh=true requires fresh data and returns typed refresh guidance when the selected root is stale or partial. Set goals=true to list each session's latest thread goal; goals mode makes query optional.",
    )
    .schema(session::message_search_schema),
    read(
        "tracedecay_sessions_for",
        "Sessions For Git Ref",
        "Find agent sessions correlated with a git artifact in the active project: sessions active on a branch or worktree, or sessions with evidence that they produced or observed a commit. Commit queries default to direct producer evidence; use relation=observed/all for weaker historical overlap. Supports time-scoped queries via since/until.",
    )
    .schema(session::sessions_for_schema),
    read(
        "tracedecay_workflows",
        "Workflow Runs",
        "Recover Claude Code workflow runs (multi-agent `wf_*` orchestrations) and their per-phase agents from the active project. Three modes, chosen by which argument is set: (1) list runs for a parent thread via session_id, or every run on a branch/worktree/commit via branch/worktree/commit (a run inherits its parent session's git spans); (2) show one run's result summary, phases, and agent roster via run_id; (3) drill into one agent's transcript via run_id + agent_label. Read-only; runs that never ran leave no rows.",
    )
    .schema(session::workflows_schema),
    read(
        "tracedecay_lcm_status",
        "LCM Status",
        "Return LCM schema, raw-message, summary, payload, and maintenance counts plus store token estimates, stored summary-depth distribution with compression ratio, payload byte totals, and payload GC status from the active project session store. Summaries are present only after authenticated host publication.",
    )
    .schema(lcm::lcm_status_schema),
    read(
        "tracedecay_lcm_doctor",
        "LCM Doctor",
        "Read a bounded, redacted temporal-store health report through the daemon-owned LCM authority without payload body exposure. This surface never repairs, cleans, or garbage-collects; daemon-owned maintenance acts on findings outside this tool.",
    )
    .schema(lcm::lcm_doctor_schema),
    read(
        "tracedecay_lcm_load_session",
        "LCM Load Session",
        "Load ordered lossless raw session messages with stable pagination and bounded content slices from the active project LCM store.",
    )
    .schema(lcm::lcm_load_session_schema),
    read(
        "tracedecay_lcm_grep",
        "LCM Grep",
        "Search bounded LCM raw-message snippets and optional summary text across all providers in the active project session store. Pass provider to constrain one provider.",
    )
    .schema(lcm::lcm_grep_schema),
    read(
        "tracedecay_lcm_describe",
        "LCM Describe",
        "Describe one session's LCM raw-message and summary-DAG shape from the active project store without exposing full payload bodies.",
    )
    .schema(lcm::lcm_describe_schema),
    read(
        "tracedecay_lcm_expand",
        "LCM Expand",
        "Expand one canonical occurrence, legacy raw-message alias, summary node, or external payload through the bounded LCM query API from the active project store.",
    )
    .schema(lcm::lcm_expand_schema),
    read(
        "tracedecay_lcm_expand_query",
        "LCM Expand Query",
        "Assemble bounded LCM retrieval context for a prompt from the active project store and, when an automation backend is configured and available, synthesize the answer directly (returned first as `answer`, with `needs_synthesis:false`). Base64 signature blobs are filtered from the context. When no backend is available it falls back to returning the raw context with `needs_synthesis:true` for the host to synthesize.",
    )
    .schema(lcm::lcm_expand_query_schema),
    read(
        "tracedecay_unsafe_patterns",
        "Risky Pattern Finder",
        "Find unwrap(), expect(), panic!(), todo!(), unimplemented!(), and unsafe \
         { } sites across the project. Each match includes the file, line, kind, \
         enclosing symbol, the source line, and an in_test flag derived from the \
         path. Use this in security/quality reviews to surface panic sites before \
         a release. Defaults to all kinds; pass `kinds` to narrow.",
    ),
    read(
        "tracedecay_config",
        "Config File Query",
        "Query TOML or JSON config files by dotted key path. Use 'path' for a \
         single file (e.g. Cargo.toml, tsconfig.json, pyproject.toml) or 'glob' \
         to query the same key across multiple files. The 'key' is dot-separated \
         (e.g. 'package.version', 'dependencies.tokio'). Returns each match's \
         file, parsed value, and the line where the key is defined. Format is \
         detected from extension: .toml → TOML, .json → JSON. \
         \n\nDoes not query the code graph, pure filesystem + parser. Works \
         on uninitialized projects.",
    ),
    read(
        "tracedecay_constructors",
        "Struct Literal Sites",
        "Find every place a given struct is instantiated as a literal \
         ({ field: value, ... }). Each result includes the file, line, the \
         explicitly initialized fields, fields supplied by Rust struct update \
         syntax (`..base`), and fields supplied by neither path relative to \
         the struct's current graph definition. Resolution remains explicitly \
         unverified because syntax alone cannot link same-name types across \
         modules; ambiguous definitions or recovered syntax report unknown \
         field coverage instead of inferred missing fields.",
    ),
    read(
        "tracedecay_field_sites",
        "Field Read/Write Sites",
        "Find every read and write site of a named field across the codebase. \
         Returns two arrays: write_sites (assignments to the field) and \
         read_sites (everything else). Each entry includes file, line, \
         enclosing symbol, and a source snippet. Useful when renaming, \
         removing, or adding an invariant to a field, the write-site list \
         is the exact blast radius. Pattern matches `.<field>` references; \
         field-by-name is shorthand for any struct's same-named field, while \
         `Struct::field` form narrows to a specific declaration.",
    ),
    write(
        "tracedecay_replace_symbol",
        "Replace Symbol Source",
        "Replace the full source of a named symbol (function, method, struct, \
         enum, etc.) with new source text. Resolves the symbol via exact \
         qualified-name match; on ambiguity, callable kinds win, and if \
         still ambiguous the edit is refused. The replaced span covers the \
         item's LEADING doc-comment / attribute block (e.g. `///` docs, `#[...]` \
         attributes) as well as its body, so `new_source` must itself include \
         any docs/attributes you want to keep, otherwise they are dropped. \
         The result's `replaced_span` returns the exact text that was swapped \
         out (docs/attrs included) so you can recover them; a `message` note \
         flags when the old span had docs/attrs the replacement appears to omit. \
         Preserves the surrounding file untouched and reindexes after writing.",
    )
    .schema(edit::replace_symbol_schema),
    write(
        "tracedecay_insert_at_symbol",
        "Insert Near Symbol",
        "Insert content immediately before or after a named symbol's source \
         range. Same resolution semantics as `tracedecay_replace_symbol`. \
         Use `position=\"before\"` or `position=\"after\"` (default: after).",
    )
    .schema(edit::insert_at_symbol_schema),
    write(
        "tracedecay_move_symbol",
        "Move Symbol Across Files",
        "Move a function (Rust-first) from its file to a destination file, and \
, the centerpiece, report the full IMPACT of the move: every caller \
         whose reference breaks, every dependency the body loses at the \
         destination, visibility that must be escalated, destination collisions, \
         and missing module declarations. Each finding is an evidence-based hint \
         { kind, file, line, detail, suggestion } derived from the code graph \
         (callers/callees) and parse-level facts (identifiers, `use` lines), not \
         regex guessing. The moved span includes the item's leading \
         doc-comment / attribute block. Defaults to a DRY RUN, the report and a \
         combined source+destination preview diff are the product; applying is \
         opt-in via `dry_run: false`, which removes the span from the source, \
         inserts it at the destination, and auto-inserts unambiguous needed \
         imports (returned in `applied_imports`). Caller references are never \
         auto-edited in v1; the exact change rides in each hint.",
    )
    .schema(edit::move_symbol_schema),
    write(
        "tracedecay_rename_symbol",
        "Rename Symbol",
        "Apply-grade rename of a graph-bound symbol across its declaration and \
         every graph reference site. Consumes the exact identity reported by \
         `tracedecay_rename_preview` (node_id, qualified_name, kind, file, \
         old_name), a bare spelling is never sufficient, and rewrites \
         whole-identifier occurrences only on graph-attested lines. Text-only \
         matches (comments, strings, dynamic dispatch, unresolved refs) are \
         reported and never rewritten. Defaults to a DRY RUN returning the \
         per-file plan, diff, and the expected_state digest; applying is \
         opt-in via `dry_run: false` with a fresh `idempotency_key`, the \
         preview's exact `accepted_preview` manifest, and that `expected_state`. \
         The apply refuses stale identity, manifest, or drifted files before any \
         write, and a partial failure restores every preimage.",
    )
    .schema(edit::rename_symbol_schema),
    write(
        "tracedecay_source_edit_reconcile",
        "Reconcile Source Edit",
        "ADMIN: conclude one retained source-edit EffectUnknown after you inspect the exact current candidate-file state yourself. This never retries the edit. Pass confirm=true as your own attestation of that inspection. Do not pause for a human to confirm a file-state conclusion you can verify.",
    )
    .schema(edit::source_edit_reconcile_schema),
    write(
        "tracedecay_source_edit_rollback",
        "Roll Back Source Edit",
        "Restore the exact private preimages retained for one completed move_symbol effect. This is a journaled inverse effect, not a second semantic move. It refuses stale or foreign workspace bytes and requires the original effect receipt identity.",
    )
    .schema(edit::source_edit_rollback_schema),
    read(
        "tracedecay_find_exact_symbol",
        "Exact Symbol Lookup",
        "Find indexed symbols whose bare name equals `name` (case-insensitive, unranked, default 20 and at most 200), with near-miss name suggestions when none match. \
         Use by_qualified_name when you have the full qualified path, or \
         `tracedecay_search` for relevance-ranked discovery.",
    ),
];
