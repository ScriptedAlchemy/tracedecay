# MCP Tool Test Queries

Manual test queries for verifying the tracedecay MCP tools. Run these in a Claude Code session after `tracedecay init` and `tracedecay install`.

### Staleness warnings

All tool responses may be prepended with staleness warnings when the index is out of date:

- **Per-file**: `WARNING: STALE INDEX — N file(s) modified since last sync: file1.rs, file2.rs. Run tracedecay sync to update.`
- **Index age**: `WARNING: Index last synced Xh Ym ago. Run tracedecay sync to update.`
- **Branch fallback**: `WARNING: branch 'feature-x' is not tracked — serving from 'main'. Run tracedecay branch add feature-x to track it.`

To test staleness: edit a file without re-syncing, then call any tool that touches that file.
To test branch fallback: check out an untracked branch while multi-branch is active, then call any tool.

---

## tracedecay_status

> What's the current status of the tracedecay index?

Expected: Returns node/edge/file counts, DB size, language distribution, tokens saved. Also includes staleness info:
- `stale_commits`: number of git commits since last sync (if > 0)
- `stale_warning`: human-readable message about stale commits
- `stale_files`: count of files modified on disk since indexing (sampled up to 100)

When multi-branch is active, also includes:
- `active_branch`: the current git branch name
- `branch_fallback`: `true` if serving from an ancestor branch DB
- `branch_warning`: explanation of which branch DB is being used

To test staleness: make a git commit without running `tracedecay sync`, then call status.

---

## tracedecay_search

> Search for symbols named "Database" in this project.

Expected: Returns matching symbols with IDs, file paths, line numbers, and signatures.

---

## tracedecay_context

> Build context for the task: "understand how the MCP server handles incoming tool calls"

Expected: Returns entry points, related symbols, relationships, and code snippets relevant to MCP tool handling.

Test with code snippets:
```
tracedecay_context(task="how does the search tool work", include_code=true, max_code_blocks=3)
```
Expected: Same as above but with source code snippets embedded for the most relevant symbols.

Test plan mode:
```
tracedecay_context(task="add a new MCP tool for dependency visualization", mode="plan", include_code=true)
```
Expected: Standard context plus additional sections:
- **Extension Points**: public traits/interfaces with implementor counts
- **Test Coverage**: test files covering the related modules

---

## tracedecay_node

> Get detailed information about the `TraceDecay` struct. First search for it, then use the node ID.

Expected: Returns full node details including qualified name, signature, docstring, visibility, line range.

---

## tracedecay_callers

> What functions call `get_tokens_saved`? Search for it first to get the node ID.

Expected: Returns caller symbols with file paths and edge types.

---

## tracedecay_callees

> What does the `run` function in main.rs call? Search for it first to get the node ID.

Expected: Returns callee symbols showing the call graph from `run`.

---

## tracedecay_impact

> What would be affected if I changed the `Database` struct? Search for it first, then compute impact.

Expected: Returns all symbols that directly or indirectly depend on `Database`.

---

## tracedecay_files

> List all indexed files under the `src/mcp/` directory.

Expected: Returns files in `src/mcp/` with symbol counts and sizes.

---

## tracedecay_affected

> If I changed `src/mcp/tools.rs` and `src/tracedecay.rs`, what test files would be affected?

Expected: Returns test files that transitively depend on those source files.

---

## tracedecay_dead_code

> Find potentially dead code — functions and methods that nothing calls.

Expected: Returns symbols with no incoming edges. Some may be entry points (main, test functions) which are expected false positives.

---

## tracedecay_diff_context

> What's the semantic context for changes to `src/cloud.rs` and `src/user_config.rs`?

Expected: Returns symbols in those files, what depends on them, and affected tests.

---

## tracedecay_module_api

> Show the public API of `src/tracedecay.rs`.

Expected: Returns all public symbols in that file with their signatures — the external interface of the TraceDecay struct.

---

## tracedecay_circular

> Are there any circular dependencies between files in this project?

Expected: Returns a list of dependency cycles (may be empty if the codebase has no circular deps).

---

## tracedecay_hotspots

> What are the most connected symbols in the codebase? Show the top 5.

Expected: Returns the 5 symbols with the highest combined incoming + outgoing edge count.

---

## tracedecay_similar

> Find symbols with names similar to "extract".

Expected: Returns symbols like `extract_python`, `extract_ruby`, `RustExtractor`, etc.

---

## tracedecay_rename_preview

> If I rename the `search` method, what would be affected? Search for it first, then preview the rename.

Expected: Returns all edges (callers, containers, etc.) referencing that symbol.

---

## tracedecay_unused_imports

> Are there any unused imports in the project?

Expected: Returns import/use nodes that have no matching references in the graph.

---

## tracedecay_changelog

> What symbols changed between the last two commits? Use `HEAD~1` and `HEAD`.

Expected: Returns a structured changelog showing added/removed/modified symbols per changed file.

---

## tracedecay_rank

> What is the most implemented interface? What class implements the most interfaces?

Test incoming (default):
```
tracedecay_rank(edge_kind="implements", node_kind="interface", limit=5)
```
Expected: Returns interfaces ranked by number of implementations (e.g. `Versioned` with 104).

Test outgoing:
```
tracedecay_rank(edge_kind="implements", direction="outgoing", node_kind="class", limit=5)
```
Expected: Returns classes ranked by how many interfaces they implement (e.g. `PartitionData` with 16).

Other useful queries:
- Most extended class: `edge_kind="extends", node_kind="class"`
- Most called function: `edge_kind="calls", node_kind="method"`
- Most annotated class: `edge_kind="annotates", direction="outgoing", node_kind="class"`

---

## tracedecay_largest

> What are the largest classes? What are the longest methods?

Test:
```
tracedecay_largest(node_kind="class", limit=5)
tracedecay_largest(node_kind="method", limit=5)
```
Expected: Returns nodes ranked by line count (end_line - start_line + 1) with start/end lines.

---

## tracedecay_coupling

> Which files are depended on by the most other files? Which files have the most outward dependencies?

Test fan-in:
```
tracedecay_coupling(direction="fan_in", limit=5)
```
Expected: Returns files ranked by how many other files depend on them.

Test fan-out:
```
tracedecay_coupling(direction="fan_out", limit=5)
```
Expected: Returns files ranked by how many other files they depend on.

---

## tracedecay_inheritance_depth

> What are the deepest class inheritance hierarchies?

Test:
```
tracedecay_inheritance_depth(limit=5)
```
Expected: Returns classes ranked by inheritance chain depth via `extends` edges. Uses recursive CTE.

---

## tracedecay_distribution

> How many classes vs interfaces vs methods are in a given package?

Test summary mode:
```
tracedecay_distribution(path="kafka/clients/src/main/java/org/apache/kafka/common/config", summary=true)
```
Expected: Returns aggregated node kind counts (e.g. 355 fields, 193 methods, 20 classes).

Test per-file mode:
```
tracedecay_distribution(path="src/mcp")
```
Expected: Returns per-file breakdown of node kinds.

---

## tracedecay_recursion

> Are there any recursive or mutually-recursive call cycles? (NASA Power of 10, Rule 1)

Test:
```
tracedecay_recursion(limit=5)
```
Expected: Returns call cycles found via DFS on the calls-only edge subgraph. Each cycle shows the chain of functions forming the loop. Self-recursive functions appear as length-1 cycles.

---

## tracedecay_complexity

> What are the most complex functions in the codebase?

Test:
```
tracedecay_complexity(limit=5)
tracedecay_complexity(node_kind="function", limit=10)
```
Expected: Returns functions/methods ranked by composite score: `lines + (fan_out × 3) + fan_in`. Shows individual metrics (lines, fan_out, fan_in) alongside the total score. Also includes real cyclomatic complexity (`branches + 1`), branch count, loop count, return count, and max nesting depth — all extracted from the AST during indexing.

---

## tracedecay_doc_coverage

> Which public symbols are missing documentation?

Test:
```
tracedecay_doc_coverage(limit=20)
tracedecay_doc_coverage(path="kafka/clients/src/main", limit=10)
```
Expected: Returns public functions, methods, classes, interfaces, traits, structs, and enums that have no docstring. Grouped by file with counts.

---

## tracedecay_god_class

> Which classes have the most members? Are there any god classes that need decomposition?

Test:
```
tracedecay_god_class(limit=5)
```
Expected: Returns classes ranked by total member count (methods + fields). Shows method count, field count, and total separately.

---

## tracedecay_port_status

> Compare porting progress between `src/python/` (source) and `src/rust/` (target).

Test:
```
tracedecay_port_status(source_dir="src/python/", target_dir="src/rust/")
```
Expected: Returns coverage summary with matched/unmatched/target-only counts. Matches by name (case-insensitive) with cross-language kind compatibility (`class` matches `struct`, `interface` matches `trait`). Unmatched symbols are grouped by source file. Shows `coverage_percent`.

Custom kinds filter:
```
tracedecay_port_status(source_dir="lib/old/", target_dir="lib/new/", kinds=["function", "method"])
```
Expected: Only compares functions and methods between the two directories.

---

## tracedecay_port_order

> What order should I port symbols from `src/python/` to minimize dependency issues?

Test:
```
tracedecay_port_order(source_dir="src/python/", limit=30)
```
Expected: Returns symbols in topological dependency order, organized into levels:
- **Level 0**: No internal dependencies (utilities, constants) — port these first
- **Level 1**: Depends only on level 0 symbols
- **Level N**: Depends on levels 0 through N-1
- **Cycles**: Mutually dependent symbols flagged as "port together"

Each symbol shows its `depends_on` list (names of dependencies within the source dir).

Custom kinds:
```
tracedecay_port_order(source_dir="src/legacy/", kinds=["function", "class"], limit=50)
```
Expected: Only includes functions and classes in the topological sort.

---

## tracedecay_commit_context

> Summarize my uncommitted changes for a commit message.

Test all changes:
```
tracedecay_commit_context()
```
Expected: Returns changed files with semantic roles (source/test/config/docs), symbols in each file, a suggested commit category (feature/fix/refactor/test/chore), and the 5 most recent commit subjects for style matching.

Test staged only:
```
tracedecay_commit_context(staged_only=true)
```
Expected: Same as above but only includes staged changes (git index vs HEAD).

If no changes: returns "No changes detected."
If not a git repo: returns a git error message.

---

## tracedecay_pr_context

> Summarize changes for a pull request from the current branch against main.

Test with defaults:
```
tracedecay_pr_context()
```
Expected: Returns semantic diff between `main` and `HEAD`:
- Commit log (hash + subject for each commit)
- Symbols added (new symbols with no external callers)
- Symbols modified (existing symbols with external callers)
- Test files changed directly
- Affected tests (transitively impacted via dependency graph)
- Impacted modules (directories containing dependents of modified symbols)

Test with custom refs:
```
tracedecay_pr_context(base_ref="develop", head_ref="feature-branch")
```
Expected: Same structure but comparing the specified refs.

---

## tracedecay_simplify_scan

> Analyze changed files for quality issues.

Test:
```
tracedecay_simplify_scan(files=["src/mcp/tools/handlers.rs", "src/mcp/tools/definitions.rs"])
```
Expected: Returns four categories of findings:
- **duplications**: symbols with >0.8 name similarity to symbols in other files
- **dead_introductions**: private functions/methods with no incoming edges (unreferenced)
- **complexity_warnings**: functions exceeding composite score threshold (lines + fan_out*3 > 100)
- **coupling_warnings**: files with fan_in > 15 (many dependents)

Each finding includes the symbol name, file, line number, and reason.

---

## tracedecay_test_map

> Which tests cover the functions in `src/tracedecay.rs`?

Test by file:
```
tracedecay_test_map(file="src/tracedecay.rs")
```
Expected: Returns:
- **coverage**: list of source functions/methods paired with their test callers (test name, file, line)
- **uncovered**: source functions/methods with no test callers found (up to depth 3)
- **test_files**: deduplicated list of all test files providing coverage
- **covered_symbols** / **uncovered_symbols**: counts

Test by node ID:
```
tracedecay_test_map(node_id="fn:search_nodes")
```
Expected: Same structure but for a single symbol. If it's not a function/method, no coverage data is returned.

---

## tracedecay_type_hierarchy

> Show the full type hierarchy for a trait. Search for the trait first, then use its node ID.

Test:
```
tracedecay_type_hierarchy(node_id="trait:McpTransport")
```
Expected: Returns an indented tree showing the root type and all implementors/extenders recursively:
```
McpTransport (trait) -- src/mcp/transport.rs:191
|- implements StdioTransport (struct) -- src/mcp/transport.rs:203
|- implements ChannelTransport (struct) -- src/mcp/transport.rs:236
```

Test with depth limit:
```
tracedecay_type_hierarchy(node_id="interface:Serializable", max_depth=2)
```
Expected: Same tree structure but stops at depth 2 (no grandchildren of grandchildren).

---

## tracedecay_branch_search

> Search for a symbol in another branch's graph without switching your checkout.

**Prerequisites:** Multi-branch must be active. Run `tracedecay branch add main` and `tracedecay branch add feature-x` first.

Test:
```
tracedecay_branch_search(branch="main", query="Database", limit=5)
```
Expected: Returns matching symbols from `main`'s graph, each tagged with `"branch": "main"`. Results may differ from the current branch if the symbol was modified or removed.

Test with untracked branch:
```
tracedecay_branch_search(branch="nonexistent-branch", query="test")
```
Expected: Returns an error: `branch 'nonexistent-branch' is not tracked`.

---

## tracedecay_branch_diff

> Compare code graphs between two branches to see what symbols were added, removed, or changed.

**Prerequisites:** Both branches must be tracked via `tracedecay branch add`.

Test with defaults (current branch vs main):
```
tracedecay_branch_diff()
```
Expected: Returns a JSON object with:
- `base`: the default branch name (e.g. "main")
- `head`: the current branch name
- `summary`: counts of added/removed/changed symbols
- `added`: symbols in head but not base (with name, kind, file, line, signature)
- `removed`: symbols in base but not head
- `changed`: symbols in both but with different signatures (shows both `base_signature` and `head_signature`)

Test with explicit branches:
```
tracedecay_branch_diff(base="main", head="feature/foo")
```
Expected: Same structure comparing the specified branches.

Test with file filter:
```
tracedecay_branch_diff(base="main", head="feature/foo", file="src/tracedecay.rs")
```
Expected: Only symbols from `src/tracedecay.rs` appear in the diff.

Test with kind filter:
```
tracedecay_branch_diff(base="main", head="feature/foo", kind="function")
```
Expected: Only function symbols appear in the diff.

Test same branch error:
```
tracedecay_branch_diff(base="main", head="main")
```
Expected: Returns an error: `base and head are the same branch: 'main'`.

---

## tracedecay_health

> How healthy is this codebase? Show the quality signal with details.

Test with defaults:
```
tracedecay_health()
```
Expected: Returns `{quality_signal: N, files_analyzed: N}` where `quality_signal` is a composite score from 0 to 10000.

Test with details:
```
tracedecay_health(details=true)
```
Expected: Same response plus a `dimensions` breakdown with five named dimensions — `acyclicity`, `depth`, `equality`, `redundancy`, `modularity` — each with a score (0.0–1.0) and supporting metrics explaining the rating.

Test with path filter:
```
tracedecay_health(path="src/mcp", details=true)
```
Expected: Same structure but scoped to files under `src/mcp/` only.

Test that `details=true` surfaces raw counts + interpretation per dimension:
```
tracedecay_health(details=true)
```
Expected: Each dimension is a `{ score, source, ... }` object. `equality` carries `gini` and an `interpretation` string. `acyclicity` carries `edges_in_cycles`. `depth` carries `max_chain` / `ideal_chain`. `modularity` carries `interpretation` and `components_after_hub_removal`. `redundancy` carries `dead_count` / `total_fns`. `coverage_discipline` carries `skip_test_coverage_count` / `total_fns`.

---

## tracedecay_redundancy

> Find functions and methods that look like duplicates of each other.

Test with defaults:
```
tracedecay_redundancy()
```
Expected: Returns `{candidates, scanned, skipped_for_size, pair_count, pairs: [...], ranked_by, scope, thresholds}`. Each pair has `similarity` (0.0-1.0), `severity` (`definite` / `likely` / `naming_only`), `overlap_kind` (`ast_isomorphic` / `control_flow` / `algorithmic` / `token_overlap` / `naming`), `a` / `b` symbol info (`file`, `line`, `name`, `id`), and a `signals` block with `ast_match`, `cfg_match`, `call_seq_match`, `shingle_jaccard`, and `body_tokens`. Pairs are sorted by similarity descending. Default thresholds: `min_lines=8`, `max_pairs=20`, `similarity_threshold=0.6`, `include_naming_only=false`.

Test path scope:
```
tracedecay_redundancy(path="src/mcp/")
```
Expected: Same structure but only candidates under `src/mcp/`. Useful for module-scoped consolidation work.

Test tightened threshold (only "definite" duplicates):
```
tracedecay_redundancy(similarity_threshold=0.85)
```
Expected: Only AST-isomorphic pairs survive — the AST signal alone contributes 0.40 to the composite score, so anything ≥ 0.85 with `severity: "definite"` is a strong consolidation candidate.

Test surface naming-only candidates:
```
tracedecay_redundancy(include_naming_only=true, similarity_threshold=0.1)
```
Expected: Long-tail matches where only names look similar but bodies diverge. Most are false positives — useful for naming-convention audits, not for refactoring.

Test minimum function size:
```
tracedecay_redundancy(min_lines=20)
```
Expected: Skips functions shorter than 20 source lines. Helps suppress noise from tiny boilerplate (getters, simple wrappers) that often look structurally identical without warranting consolidation.

Cost model:
- First call on a fresh index parses each candidate file once and writes fingerprints to the `node_fingerprints` table (schema v10).
- Subsequent calls reuse cached fingerprints when the body source hash is unchanged.
- Pairwise comparison is bucketed by `body_tokens` (±25 % window), so cost is sub-quadratic on large repos.

Cross-language note: signals derive from raw tree-sitter kind names. Two duplicates only match within the same language — cross-language matching (e.g. a Python helper and its TypeScript twin) is intentionally out of scope.

---

## tracedecay_runtime

> Capture process + database telemetry for triaging unexpected CPU or RAM consumption (issue #80).

Test:
```
tracedecay_runtime()
```
Expected: Returns a snapshot with the shape:
```
{
  "captured_at": <unix-seconds>,
  "tracedecay_version": "6.0.0",
  "host_os": "macos" | "linux" | "windows" | ...,
  "process": {
    "pid": ...,
    "rss_bytes": ...,
    "virtual_bytes": ...,
    "cpu_percent": ...,       // sampled over a 200 ms window
    "uptime_secs": ...,
    "system_cpu_count": ...,
    "system_total_memory_bytes": ...
  },
  "database": {
    "project_root": "...",
    "db_path": "...",
    "db_size_bytes": ...,
    "wal_size_bytes": ...,
    "shm_size_bytes": ...,
    "journal_mode": "wal" | "delete" | ...,
    "source_total_bytes": ...,
    "node_count": ...,
    "edge_count": ...
  }
}
```

`cpu_percent` is the delta between two `sysinfo` refreshes 200 ms apart — values above 100 are legitimate on multi-threaded workloads; divide by `system_cpu_count` to get "fraction of total host CPU."

Latency note: the tool blocks for ~200 ms because the CPU% sample needs two readings around a sleep. This is intentional — a one-shot read would always report 0.

CLI equivalent (for offline reporters who can't call MCP tools):
```
tracedecay status --runtime           # human-readable text
tracedecay status --runtime --json    # machine-parseable JSON, same shape
```

Diagnostic ratios worth eyeballing:
- `database.db_size_bytes / database.source_total_bytes` — values much greater than ~10 suggest WAL/checkpoint bloat or retained history.
- `database.wal_size_bytes` / `database.shm_size_bytes` vs `database.db_size_bytes` — a WAL larger than the DB itself usually means a checkpoint hasn't run recently.
- `process.rss_bytes / process.system_total_memory_bytes` — high values explain "everything got slow" symptoms.

---

## tracedecay_gini

> How evenly is complexity distributed across files? Are there any god files?

Test default:
```
tracedecay_gini()
```
Expected: Returns `{gini: 0.XX, interpretation: "...", total_items: N, metric: "complexity", scope: "file", outliers: [...]}`. A Gini coefficient close to 1.0 indicates high inequality (a few files dominate).

Test alternative metrics:
```
tracedecay_gini(metric="lines")
tracedecay_gini(metric="fan_in")
```
Expected: Same structure ranked by lines or fan-in instead of complexity.

Test per-symbol scope:
```
tracedecay_gini(metric="complexity", scope="symbol")
```
Expected: Same structure but computes inequality across individual symbols rather than files.

Test members:
```
tracedecay_gini(metric="members")
```
Expected: Scope is forced to `symbol`; counts methods and fields per class/struct to surface god-class candidates.

---

## tracedecay_dependency_depth

> What are the longest dependency chains in the codebase?

Test with limit:
```
tracedecay_dependency_depth(limit=5)
```
Expected: Returns `{max_depth: N, ideal_depth: N, depth_score: 0.XX, chains: [{file, depth, chain: [...]}]}` showing the five deepest transitive import chains.

Test with path filter:
```
tracedecay_dependency_depth(path="src/mcp")
```
Expected: Same structure but only considers files under `src/mcp/` as roots.

---

## tracedecay_dsm

> Show me the design structure matrix — how do files depend on each other?

Test stats (default):
```
tracedecay_dsm()
tracedecay_dsm(format="stats")
```
Expected: Returns `{files: N, edges: N, density: 0.XXX, clusters: N, largest_cluster: {name, files}}` — a high-level summary of file coupling.

Test clusters:
```
tracedecay_dsm(format="clusters")
```
Expected: Returns `{clusters: [{name, files: [...], internal_edges, outgoing_edges, incoming_edges}]}` — each strongly-connected cluster listed with its coupling metrics.

Test matrix:
```
tracedecay_dsm(format="matrix", max_files=15)
```
Expected: Returns `{files: [...short names...], matrix: [[NxN]], note: "..."}` — a compact NxN adjacency matrix where entry `[i][j]` is non-zero when file `i` depends on file `j`.

---

## tracedecay_test_risk

> Where should I write the next test? What's the riskiest untested code?

Test with limit:
```
tracedecay_test_risk(limit=10)
```
Expected: Returns `{risks: [{symbol, file, line, complexity, fan_in, has_test, risk_score, churn}], summary: {total_functions, tested, coverage_pct, top_risk_untested}}`. Results are sorted by `risk_score` descending; untested symbols appear first by default.

Test with path filter:
```
tracedecay_test_risk(path="src/mcp", limit=5)
```
Expected: Same structure but scoped to functions under `src/mcp/`.

Test include tested:
```
tracedecay_test_risk(include_tested=true, limit=5)
```
Expected: Also returns already-tested symbols ranked by risk score — useful for identifying weak-test candidates (high-risk code that has a test but may need more coverage).

---

## tracedecay_session_start

> Save a health baseline before I start working.

Test:
```
tracedecay_session_start()
```
Expected: Returns `{status: "baseline_saved", quality_signal: N, files_analyzed: N}`. Also writes `.tracedecay/session_baseline.json` in the project root with the full health snapshot for later comparison.

---

## tracedecay_session_end

> Compare current health against the baseline — did my changes degrade the codebase?

Test after a prior `tracedecay_session_start`:
```
tracedecay_session_end()
```
Expected: Returns `{pass: true/false, signal_before: N, signal_after: N, delta: N, files_analyzed: N, degraded_dimensions: [...], dimensions: {per_dim with before/after/delta/direction}}`. The baseline file is removed after `session_end` completes.

Test without a baseline:
```
tracedecay_session_end()
```
Expected: Returns `{status: "no_baseline", message: "No session baseline found. Call tracedecay_session_start first."}`.

---

## tracedecay_read

> Read a file with mode-aware compression. Modes: `full`, `lines`, `map`, `signatures`. Cross-session cached.

Test full content:
```
tracedecay_read(file="src/sync.rs", mode="full")
```
Expected: Returns the entire file body, plus `mtime_ns`, `digest`, and `token_count`.

Test line slice:
```
tracedecay_read(file="src/sync.rs", mode="lines", lines="120-180")
```
Expected: Returns only the requested 1-based inclusive range.

Test map (graph-only, no source bytes touched):
```
tracedecay_read(file="src/sync.rs", mode="map")
```
Expected: Flat list of every top-level symbol with `kind`, `name`, `line`, `end_line`, `visibility`.

Test signatures:
```
tracedecay_read(file="src/sync.rs", mode="signatures")
```
Expected: Functions and types with their cached signature strings.

Test cache hit (call the same query twice):
```
tracedecay_read(file="src/sync.rs", mode="full")  # populates cache
tracedecay_read(file="src/sync.rs", mode="full")  # second call
```
Expected: The second call returns `{"unchanged": true, "digest": ..., "mtime_ns": ..., "token_count": ...}` — a small stub instead of the full body.

---

## tracedecay_outline

> Flat list of every top-level symbol in a file, with optional kind filter.

Test default (all kinds):
```
tracedecay_outline(file="src/mcp/tools/handlers/info.rs")
```
Expected: Returns `{file, symbol_count, symbols: [{kind, name, line, end_line, visibility}]}` sorted by line.

Test kinds filter:
```
tracedecay_outline(file="src/mcp/tools/handlers/info.rs", kinds=["function"])
```
Expected: Only function-kind entries. Filter is case-insensitive (`["FUNCTION"]` works the same).

Unknown kind returns empty:
```
tracedecay_outline(file="src/mcp/tools/handlers/info.rs", kinds=["banana"])
```
Expected: `symbol_count: 0`.

---

## tracedecay_implementations

> Find every type implementing a given trait, or every body of a given method name.

Test trait form:
```
tracedecay_implementations(trait="LanguageExtractor")
```
Expected: For each implementing type, returns the type name, file, line, the trait name, and an array of method bodies (signature + body for each method on the impl).

Test method form:
```
tracedecay_implementations(method="extensions")
```
Expected: Every Function/Method node named `extensions` with full body. Useful for cross-impl comparisons.

Errors:
```
tracedecay_implementations()                          # no args → error
tracedecay_implementations(trait="X", method="y")     # both args → error (mutually exclusive)
```

---

## tracedecay_unsafe_patterns

> Surface unwrap, expect, panic!, todo!, unimplemented!, and unsafe { } sites.

Test all kinds (default):
```
tracedecay_unsafe_patterns()
```
Expected: Returns `{match_count, by_kind: {...}, matches: [{kind, file, line, snippet, enclosing, in_test}]}`. AST-style word-boundary matching — `.unwrap_or` does NOT match the `unwrap` kind.

Test exclude tests:
```
tracedecay_unsafe_patterns(exclude_tests=true)
```
Expected: Filters out files whose path looks like a test (`tests/`, `_test.rs`, `__tests__/`, etc.).

Test specific kinds:
```
tracedecay_unsafe_patterns(kinds=["panic", "unsafe_block"])
```
Expected: Only panic and unsafe-block matches.

Path scope:
```
tracedecay_unsafe_patterns(kinds=["unwrap"], path="src/mcp/")
```
Expected: Only matches under `src/mcp/`.

---

## tracedecay_diagnostics

> Run the project's compile/type checker and return structured errors mapped to graph nodes.

Test workspace (default scope):
```
tracedecay_diagnostics()
```
Expected: For Rust projects, runs `cargo check --message-format=json --target-dir .tracedecay/target`. Returns `{scope, diagnostic_count, error_count, warning_count, diagnostics: [{file, line_start, line_end, level, code, message, driver, enclosing}]}`.

For TypeScript projects (tsconfig.json present), runs `tsc --noEmit --pretty false`. For Python projects (pyproject.toml or pyrightconfig.json present), runs `pyright --outputjson`.

Mixed-language projects run every detected driver and merge results.

Test package scope (Rust only):
```
tracedecay_diagnostics(scope="package", name="tracedecay")
```
Expected: `cargo check -p tracedecay` rather than the full workspace.

Test file scope:
```
tracedecay_diagnostics(scope="file", path="src/lib.rs")
```
Expected: Workspace check + post-filter to the requested file.

Errors:
```
tracedecay_diagnostics(scope="package")               # missing name → error
tracedecay_diagnostics(scope="file")                  # missing path → error
tracedecay_diagnostics(scope="lunch")                 # unknown scope → error
```

If a tool isn't installed (tsc, pyright), the driver returns no diagnostics rather than failing.

---

## tracedecay_config

> Query TOML or JSON config files by dotted key path.

Test single file:
```
tracedecay_config(path="Cargo.toml", key="package.version")
```
Expected: Returns `{match_count: 1, matches: [{file, key, value, line}]}`. The line number is heuristic — finds the row where the final key segment is defined.

Test JSON:
```
tracedecay_config(path="tsconfig.json", key="compilerOptions.target")
```
Expected: Same shape; the `value` field carries the parsed JSON value.

Test glob across the workspace:
```
tracedecay_config(glob="**/Cargo.toml", key="package.name")
```
Expected: One match per matching file.

Test missing key:
```
tracedecay_config(path="Cargo.toml", key="package.no_such_field")
```
Expected: `match_count: 0`, the entry has `found: false`.

Errors:
```
tracedecay_config(key="x")                            # missing path/glob → error
tracedecay_config(path="a", glob="b", key="x")        # both → error
tracedecay_config(path="a")                           # missing key → error
```

This tool is DB-free; it works on uninitialized projects.

---

## tracedecay_signature_search

> Search functions and methods by signature shape: return type, params, async.

Test by return type:
```
tracedecay_signature_search(returns="Result<")
```
Expected: Every function whose signature contains `Result<` after `->`. Returns `{match_count, matches: [{name, qualified_name, kind, file, line, is_async, signature}]}`.

Test by params:
```
tracedecay_signature_search(params=["&mut self"])
```
Expected: Every method whose parameter list contains `&mut self`. Multiple params are AND-composed.

Test async only:
```
tracedecay_signature_search(async=true)
```
Expected: Every async function/method. Set `async=false` to exclude them.

Combined filters:
```
tracedecay_signature_search(params=["&mut self"], async=true, returns="i32")
```
Expected: Only methods that match all three.

Path scope:
```
tracedecay_signature_search(returns="Result<", path="src/mcp/")
```
Expected: Only symbols defined under `src/mcp/`.

Errors:
```
tracedecay_signature_search()                         # no filters → error
```

---

## tracedecay_constructors

> Find every literal-instantiation site of a struct, plus missing fields per site.

Test:
```
tracedecay_constructors(struct="GraphStats")
```
Expected: Returns `{struct, expected_fields, match_count, sites: [{file, line, fields, missing_fields}]}`. Each site lists the fields actually present in that literal; `missing_fields` lists fields the struct has but this literal doesn't.

After adding a required field, this surfaces every site that needs updating before cargo even compiles.

Test unknown struct:
```
tracedecay_constructors(struct="DoesNotExist")
```
Expected: Returns "No struct, class, or case-class named ...".

Pattern-matching sites (`match Foo { ... }`, `if let Foo { ... }`) are filtered out, as are definition sites (`struct Foo { ... }`, `impl Foo { ... }`, `-> Foo {`). String- and char-literal occurrences (`"Foo { x: 1 }"`) are also skipped.

Errors:
```
tracedecay_constructors()                             # missing struct → error
```

---

## tracedecay_field_sites

> Partition every reference to a field into reads and writes.

Test default:
```
tracedecay_field_sites(field="last_sync_at")
```
Expected: Returns `{field, qualifier, qualifier_applied, write_count, read_count, write_sites: [...], read_sites: [...]}`. Writes include simple assignments (`x.field = ...`), compound assignments (`x.field += ...`), and `&mut x.field` borrows. Everything else is a read; `==` and `=>` do NOT count as writes.

Test writes only:
```
tracedecay_field_sites(field="last_sync_at", writes_only=true)
```
Expected: Returns only `write_sites`; `read_sites` is omitted entirely.

Test qualified form:
```
tracedecay_field_sites(field="GraphStats::last_sync_at")
```
Expected: The `qualifier` field carries `"GraphStats"` but `qualifier_applied` is `false` — the scan uses the bare field name because the tool has no type information to disambiguate `.foo` to a specific struct.

Errors:
```
tracedecay_field_sites()                              # missing field → error
```

---

> **Note:** Most tools are read-only and safe to call in parallel. The exceptions mutate state and should not be parallelised: the edit tools (`tracedecay_str_replace`, `tracedecay_multi_str_replace`, `tracedecay_insert_at`, `tracedecay_insert_at_symbol`, `tracedecay_replace_symbol`, `tracedecay_ast_grep_rewrite`) modify source files; the session and memory tools (`tracedecay_session_start`, `tracedecay_session_end`, `tracedecay_fact_store`, `tracedecay_fact_feedback`, `tracedecay_memory_status`) write to `.tracedecay/`; and `tracedecay_run_affected_tests` runs a `cargo` test subprocess.
