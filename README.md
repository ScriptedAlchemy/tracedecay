<p align="center">
  <img src="src/resources/logo.png" alt="TraceDecay" width="300">
</p>

<h3 align="center">Semantic code intelligence for AI coding agents</h3>

<p align="center"><strong>Fewer tokens &bull; fewer tool calls &bull; local by default</strong></p>

<p align="center">
  <a href="https://github.com/ScriptedAlchemy/tracedecay/releases/latest"><img src="https://img.shields.io/github/v/release/ScriptedAlchemy/tracedecay" alt="GitHub release"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-1.70+-orange.svg" alt="Rust"></a>
  <img src="https://img.shields.io/badge/macOS-supported-blue.svg" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-supported-blue.svg" alt="Linux">
  <img src="https://img.shields.io/badge/Windows-supported-blue.svg" alt="Windows">
</p>

TraceDecay builds a local semantic graph of your repository so AI coding agents can ask for the right symbols, call relationships, impact radius, docs, and source snippets without scanning the tree.

Instead of repeated `grep`, `glob`, and file reads, agents use MCP tools such as `tracedecay_context`, `tracedecay_search`, `tracedecay_callers`, and `tracedecay_impact`.

## Highlights

- 70+ MCP tools for discovery, call graphs, impact analysis, code health, test mapping, PR context, and anchored edits.
- 50+ languages through Rust tree-sitter extractors, with lite/medium/full Cargo feature tiers.
- Native integrations for Claude Code, Codex, Cursor, Gemini, Hermes, Kiro, OpenCode, Copilot, Cline, Roo Code, Zed, Antigravity, Kilo, Kimi, and Vibe.
- Local libSQL storage. Your code and project memory stay on your machine.
- On-demand freshness checks, optional per-branch databases, and linked git worktree support.
- Local dashboard for code graph, memory, LCM sessions, savings, and cost analytics.

## Install

```bash
# Linux and Apple silicon macOS
curl -fsSL https://github.com/ScriptedAlchemy/tracedecay/releases/latest/download/install.sh | bash

# Windows
scoop bucket add tracedecay https://github.com/ScriptedAlchemy/scoop-bucket
scoop install tracedecay
```

The installer defaults to `~/.local/bin`. Set `TRACEDECAY_INSTALL_DIR` to
choose another directory. Prebuilt archives are available on the
[latest release](https://github.com/ScriptedAlchemy/tracedecay/releases/latest).

## Quick Start

```bash
cd /path/to/your/project
tracedecay init
tracedecay install
tracedecay status
```

`tracedecay install` auto-detects supported agents. To target one host:

```bash
tracedecay install --agent claude
tracedecay install --agent codex
tracedecay install --agent cursor
tracedecay install --agent gemini
tracedecay install --agent hermes
```

Project-local setup:

```bash
tracedecay install --local --agent cursor
tracedecay install --local --agent codex
```

After setup, restart the agent so it loads the MCP server, plugin, hooks, or rules written for that host.

Codex first-time installs print one extra step:

```bash
codex plugin add tracedecay@personal
```

Run it once before starting a new Codex session so Codex copies the generated plugin into its installed cache.

## Common Commands

```bash
tracedecay init [path]              # initialize a project store
tracedecay sync [path]              # incremental index update
tracedecay sync --force [path]      # full re-index
tracedecay status [path]            # graph stats, freshness, savings, cost
tracedecay query <search> [path]    # CLI symbol search
tracedecay files                    # indexed files
tracedecay affected <files...>      # impacted tests/files
tracedecay serve                    # MCP server
tracedecay doctor [--agent NAME]    # installation health check
tracedecay dashboard [--open]       # local dashboard
tracedecay monitor                  # live MCP savings/cost TUI
tracedecay update                   # refresh binary, plugins, daemon
tracedecay upgrade                  # self-upgrade current channel
```

## Agent Tools

The MCP server exposes tools grouped around normal coding workflows:

- Discovery: `tracedecay_context`, `tracedecay_search`, `tracedecay_outline`, `tracedecay_files`
- Graph traversal: `tracedecay_callers`, `tracedecay_callees`, `tracedecay_impact`, `tracedecay_affected`
- Code health: `tracedecay_complexity`, `tracedecay_dead_code`, `tracedecay_coupling`, `tracedecay_test_risk`
- Git workflow: `tracedecay_diff_context`, `tracedecay_pr_context`, `tracedecay_changelog`, `tracedecay_test_map`
- Editing: `tracedecay_str_replace`, `tracedecay_multi_str_replace`, `tracedecay_insert_at`, `tracedecay_ast_grep_rewrite`
- Memory: `tracedecay_fact_store`, `tracedecay_fact_feedback`, `tracedecay_memory_status`

Most read tools are safe to call in parallel. Edit tools are single-file, anchored, and re-index after writing.

## Index Freshness

TraceDecay does not run a filesystem watcher. MCP calls check for stale indexed files and sync them on demand with a cooldown. When an MCP server starts, it runs a catch-up sync for changes made while no agent was attached.

Linked git worktrees share the same project enrollment through the repository common directory. Initialize once from any checkout; do not copy `.tracedecay/` into worktrees.

Optional branch databases:

```bash
tracedecay branch add
tracedecay branch list
tracedecay branch remove <name>
tracedecay branch gc
```

See [docs/BRANCHING-USER-GUIDE.md](docs/BRANCHING-USER-GUIDE.md) for full branch behavior and recovery.

## Dashboard

```bash
tracedecay dashboard
tracedecay dashboard --port 8080
tracedecay dashboard --port 0 --open
```

The dashboard includes graph exploration, project memory, LCM session search, token savings, and cost views. See [docs/dashboard.md](docs/dashboard.md) and [docs/graph-explorer.md](docs/graph-explorer.md).

## Privacy

Core indexing, graph queries, MCP tools, memory, and dashboard data are local.

Optional or external network calls:

- Worldwide counter: uploads one aggregate token-savings number only when enabled; the Worker also derives country from request metadata for aggregate geography.
- Version check: fetches release metadata.
- Pricing refresh: fetches public LiteLLM model pricing for `tracedecay cost`.

Disable the worldwide counter with:

```bash
tracedecay disable-upload-counter
```

## Troubleshooting

```bash
tracedecay doctor
tracedecay status --runtime
tracedecay sync --doctor
```

Common fixes:

- Not initialized: run `tracedecay init` from the project root.
- Agent does not see tools: run `tracedecay doctor --agent <name>`, then restart the agent.
- Missing symbols: run `tracedecay sync` and confirm the file is not ignored.
- Slow first index: use normal incremental `tracedecay sync` after the first run.

## Build

```bash
cargo build --release
cargo build --release --features medium
cargo build --release --no-default-features

cargo nextest run --workspace --no-fail-fast
cargo check --no-default-features
cargo clippy --workspace --all-targets
```

## Docs

- [User guide](docs/USER-GUIDE.md)
- [Comparable tools](docs/COMPARABLE-TOOLS.md)
- [Dashboard](docs/dashboard.md)
- [Branching](docs/BRANCHING-USER-GUIDE.md)
- [MCP extensions](docs/MCP-extensions.md)
- [Architecture/design notes](docs/DESIGN-DOC.md)

## Origin

TraceDecay is a Rust port of the original [CodeGraph](https://github.com/colbymchenry/codegraph) TypeScript implementation by [@colbymchenry](https://github.com/colbymchenry).

## License

MIT License -- see [LICENSE](LICENSE).
