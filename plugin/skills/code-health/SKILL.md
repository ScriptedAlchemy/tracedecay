---
name: code-health
description: 'Use when producing code-health scorecards, architecture maps, project identity/index status, registered project lookup, cross-project or cross-repo context gathering, TODO/config/runtime checks, or before/after refactor health deltas.'
---

# Code health, architecture & status

Lead with the one composite signal, then drill only into the weak dimensions
and the specific scans the user asked for — don't run every tool by reflex.

## Quality scorecard

1. **Composite signal → `tracedecay_health` / `tracedecay_health_read`**
   (`details: true`, optional `path`): the 0–10000 score plus the 5-dimension
   breakdown (acyclicity, depth, equality, redundancy, modularity) and the
   `coverage_discipline` penalty. The weak dimensions choose the drill-downs.
2. **Inequality / god files → `tracedecay_gini`** (`metric`:
   `complexity`|`lines`|`fan_in`|`fan_out`|`members`, `scope`, `path?`).
3. **Complexity & size offenders:** `tracedecay_complexity`,
   `tracedecay_largest`, `tracedecay_god_class`, `tracedecay_hotspots`.
4. **Structure drill-downs (match the weak dimension):** acyclicity →
   `tracedecay_circular` + `tracedecay_recursion`; modularity →
   `tracedecay_dsm` (`format`: `stats`|`clusters`|`matrix`) +
   `tracedecay_coupling`; depth → `tracedecay_dependency_depth` +
   `tracedecay_inheritance_depth`; relationships → `tracedecay_rank`
   (`edge_kind` required); kind mix → `tracedecay_distribution`.
5. **Duplication → `tracedecay_redundancy`**; near-duplicate names →
   `tracedecay_similar`. **Doc gaps → `tracedecay_doc_coverage`**. **Panic
   sites → `tracedecay_unsafe_patterns`**. **Risk-weighted test gaps →
   `tracedecay_test_risk`**. **Changed-files-only pass →
   `tracedecay_simplify_scan`** (`files`).
6. **Files nothing loads → `tracedecay_unmounted_files`** (`path?`, `limit?`):
   source files no build root reaches. No compiler, bundler, or test runner
   ever sees them, yet their symbols inflate every count above — resolve them
   before trusting a scorecard.

## Architecture map

1. **Shape & size:** `tracedecay_status` (node/edge/file counts),
   `tracedecay_files` + `tracedecay_distribution` (what lives where).
2. **Public surface:** `tracedecay_module_api` per top-level directory.
3. **Dependency structure:** `tracedecay_dsm` (clusters and layering
   violations), `tracedecay_coupling` (`fan_in`/`fan_out` hubs),
   `tracedecay_circular` (cycles), `tracedecay_dependency_depth` (fragile
   long chains).

## Session health delta

1. **Before the first edit → `tracedecay_health_delta`** without
   `before_cursor`: pins the current project health and returns its stable
   `after_cursor`.
2. **After the work → `tracedecay_health_delta`** with that cursor as
   `before_cursor`: returns the exact per-dimension diff. A dropped dimension
   names the follow-up (redundancy fell → `tracedecay_redundancy`; acyclicity
   fell → `tracedecay_circular`).
3. Keep the returned cursor with the task evidence; it is project- and
   path-scope-bound and does not rely on a mutable session baseline file.

## Project & index status

1. **Active project → `tracedecay_active_project`** (no args): resolved
   project root, scope prefix, branch identity, and the resolved active project store
   backing this session. Use this before describing where data lives.
2. **Storage status → `tracedecay_storage_status`** (no args): resolved
   active project store health, graph DB path, writability, branch-fallback
   warnings — instead of probing `.tracedecay` or direct SQLite checks.
3. **Project registry / cross-project context → `tracedecay_project_list` /
   `tracedecay_project_search` / `tracedecay_project_context`** when the user
   asks about another project, sibling workspace, cross-repo context, or
   cross-project context gathering. Confirm the target store first, then pass
   `project_id`, `project_path`, or `project_selector` to
   `tracedecay_context`, `tracedecay_search`, or `tracedecay_message_search`
   instead of scanning parent directories.
4. **Index status → `tracedecay_status`**: node/edge/file counts, DB size,
   active branch + fallback warning, tokens saved.
5. **Config lookups → `tracedecay_config`** (`key` required, plus `path` or
   `glob`): query TOML/JSON by dotted key — works even before `tracedecay init`.
6. **Outstanding work → `tracedecay_todos`** (`kinds?`, `path?`, `limit?`).
7. **Server triage → `tracedecay_runtime`** (PID, memory, CPU%, DB sizes) when
   TraceDecay seems to hog CPU or RAM. **Visual → `tracedecay_dashboard`**
   (`action`: `start`|`stop`): hand the URL to the user.

## Guardrails

- Discovery/analysis tools are read-only and parallel-safe. Use
  `tracedecay_health_delta` for generation-bound health evidence.
  `tracedecay_dashboard` starts/stops a local server — use it only when relevant
  and respect Cursor approval/run-mode.
- `tracedecay_redundancy` is computed lazily and cached; the first call on a
  fresh index can be slow — keep `path`/`max_pairs` tight.
- For large audits, use scoped read-only subagents by path, weak health
  dimension, or top-level directory; keep `session_start`/`session_end` in
  the parent agent.
- This skill reports and prioritizes; it does not edit. Hand fixes to
  `tracedecay:editing-safely` / `tracedecay:reviewing-changes`, verification
  to `tracedecay:assessing-impact`. Memory recall belongs to
  `tracedecay:project-memory`; past-session recall to
  `tracedecay:managing-session-context`.

## If tools are deferred or MCP fails

- Deferred (names listed without schemas): load once with ToolSearch —
  `select:tracedecay_health,tracedecay_gini,tracedecay_dsm,tracedecay_status,tracedecay_active_project,tracedecay_project_list,tracedecay_project_search,tracedecay_project_context`
  (one batched call, add others needed) — then call normally.
- MCP error/timeout/disconnect: same tool, same args, via shell:
  `tracedecay tool <name>` (see `tracedecay:using-the-cli`). Never
  query `.tracedecay` databases directly; never abandon the graph over transport.

## Deliverable

Do not end this workflow without: the composite score + weak dimensions with
ranked worst offenders and a prioritized fix list; the layered module map with
dependency hotspots/violations; the per-dimension session delta; or the status
numbers, config values (with file + line), marker list, or runtime snapshot
the user asked for. Pairs with the `docs-canvas` plugin if installed. Report
any `tracedecay_metrics:` line to the user.
