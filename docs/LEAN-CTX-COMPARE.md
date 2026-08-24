# tracedecay vs lean-ctx

Both projects compress context for AI coding agents but with different centers of gravity:

- **tracedecay** — code-graph engine (libSQL + tree-sitter, 50+ languages). 70+ MCP tools focused on symbol-level intelligence (callers/callees, impact, complexity, DSM, test_risk, code-health composite, branch diffs, atomic edit primitives). Cost tracking, on-demand index freshness, monitor TUI.
- **lean-ctx** — context runtime that *also* compresses arbitrary file reads and shell output. ~56 MCP tools plus 95+ shell-hook patterns, multi-mode file reads, hybrid search with embeddings, portable `.lctxpkg` bundles, persistent knowledge facts.

The two overlap in graph/impact analysis but diverge on read modes, shell-output compression, and persistent knowledge. TraceDecay is deeper on graph quality metrics; lean-ctx is broader on the I/O surface.

---

## Useful features to import (ranked)

### High value

1. **Mode-aware file read primitive** — `tracedecay_read` with modes `full | map | signatures | diff | lines:N-M | entropy | auto`. tracedecay already has the symbol graph (`tracedecay_node`, `tracedecay_module_api`) but no whole-file `Read` replacement. Exposing one would let agents skip raw `Read` for huge files. The `signatures` and `map` modes can be served almost for free from the existing graph.

2. **Shell-output compression patterns** — lean-ctx's 95+ declarative patterns for `git` / `cargo` / `npm` / `docker` output. Orthogonal to tracedecay's graph; addresses the *other half* of agent token spend (Bash tool results). Could ship as `tracedecay compress -c <cmd>` plus a Claude Code Bash post-tool hook. Pattern registry stays declarative and easy to extend.

3. **Hybrid search with RRF** — extend `tracedecay_search` / `tracedecay_context` with Reciprocal Rank Fusion over (FTS5 BM25, graph proximity, optional local embeddings). tracedecay already has the first two; adding a small embedding model behind a feature flag would meaningfully improve recall on conceptual queries (the `keywords` arg on `tracedecay_context` is the manual workaround for the same problem).

4. **Persistent knowledge facts** — `knowledge remember / recall / search / export / import` with category/key. Distinct from tracedecay's `session_start` / `session_end` (which are *health-metric* snapshots, not free-form facts). Useful for "the test command is X", "this module owner is Y" — survives across sessions and could be exposed as an MCP tool plus a `tracedecay://knowledge` resource.

5. **Read/result caching layer** — lean-ctx's "cached re-reads compress to ~13 tokens." For MCP responses keyed by `(file, mtime, args)`, return a tiny "unchanged since last call" stub. Lowers token cost on revisits without changing tool semantics.

### Medium value

6. **Portable context packages (`.lctxpkg`)** — SHA-256-stamped bundle of `{knowledge, graph subset, session, gotchas}`. tracedecay already produces per-branch DBs; a portable export/import format would help team sharing and CI ("seed the cache"). Naturally pairs with #4.

7. **PR context packs as artifacts** — wrap the existing `tracedecay_pr_context` output into a saveable bundle (changed files + related tests + impact + diff context) so it can be attached to PR descriptions or CI artifacts.

8. **Weekly "wrapped" report** — `tracedecay wrapped --week`. Light addition over `tracedecay cost` / `monitor` that surfaces top files, top tools, peak-savings days. Strong UX hook for users.

9. **Compaction-survival session recovery** — structured queries the agent can run after Claude's auto-compaction to rehydrate task state. tracedecay's `session_*` could grow a `tracedecay_session_recover` companion that emits a deterministic "what was I doing" summary.

10. **Cross-file block dedup** — `tracedecay_simplify_scan` finds duplications in *changed* files; lean-ctx's `ctx_dedup` does it cross-repo. Could be added as `tracedecay_dedup` over the existing AST data.

11. **Directory tree tool** — `tracedecay_tree` returning a compact directory outline. Cheap to add from the existing `files` index; saves an agent from running `find` / `ls`.

### Lower value / situational

12. **Streamable HTTP MCP transport** — `tracedecay serve --http` for clients that don't speak stdio. Useful for browser-based or remote agents; less urgent for the current CLI-agent userbase.

13. **API route extraction** — surface HTTP endpoints (e.g., axum / express / flask handlers) as a first-class node kind. Niche but high-leverage when present.

14. **Smart-read intent routing** — `auto` mode that picks `signatures` vs `full` vs `diff` from task hints. Pairs with #1; not worth adding alone.

---

## Things to skip

- **Multi-agent handoff / share / workflow tools** — tracedecay is deliberately a backend, not an orchestrator. Adding these would blur scope.
- **Sandboxed shell execution (`ctx_execute`)** — Claude Code already runs Bash; duplicating it inside the MCP server invites support burden without obvious payoff.
- **`ctx_heatmap` / agent telemetry tools** — `tracedecay monitor` and `tracedecay cost` already cover this lane.

---

## Sources

- tracedecay: `README.md`, `src/mcp/tools/definitions.rs`
- lean-ctx: <https://github.com/yvgude/lean-ctx> (`README.md`, `LEANCTX_FEATURE_CATALOG.md`)
