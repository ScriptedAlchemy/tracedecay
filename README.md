<p align="center">
  <img src="src/resources/logo.png" alt="TraceDecay" width="300">
</p>

<h3 align="center">Semantic code intelligence for AI coding agents</h3>

<p align="center"><strong>Fewer tokens &bull; fewer tool calls &bull; local by default</strong></p>

<p align="center">
  <a href="https://crates.io/crates/tracedecay"><img src="https://img.shields.io/crates/v/tracedecay.svg" alt="crates.io"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-1.85+-orange.svg" alt="Rust"></a>
  <img src="https://img.shields.io/badge/macOS-supported-blue.svg" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-supported-blue.svg" alt="Linux">
  <img src="https://img.shields.io/badge/Windows-supported-blue.svg" alt="Windows">
</p>

TraceDecay builds a local semantic graph of your repository so AI coding agents can ask for the right symbols, call relationships, impact radius, docs, and source snippets without scanning the tree.

Instead of repeated `grep`, `glob`, and file reads, agents use MCP tools such as `tracedecay_context`, `tracedecay_search`, `tracedecay_callers`, and `tracedecay_impact`.

## Highlights

- Typed MCP operations for discovery, call graphs, impact analysis, code health, test mapping, repository context, and anchored edits.
- Rust tree-sitter extractors with lite/medium/full Cargo feature tiers; `Cargo.toml` is the exact language authority.
- Native integrations for supported Claude Code, Codex, Cursor, Hermes, Kiro, Kimi Code, OpenCode, and Cline-family hosts.
- Daemon-owned local storage through the `rusqlite` runtime and embedded Grafeo graph store. Local operation is the default; configured remote sources and authorities follow explicit policy rather than an implicit local-only guarantee.
- On-demand freshness checks with exact repository, worktree, ref, commit, and generation provenance across linked git worktrees.
- Project-wide facts, sessions, and lossless LCM history remain available across branches; historical host transcripts enter through the same sanitized daemon ingestion path.
- Local dashboard for evidence, memory, LCM sessions, savings, and cost analytics.

## Install

```bash
# macOS
brew install ScriptedAlchemy/tap/tracedecay

# Windows
scoop bucket add tracedecay https://github.com/ScriptedAlchemy/scoop-bucket
scoop install tracedecay

# Any platform with Rust
cargo install tracedecay

# Smaller language tiers
cargo install tracedecay --features medium
cargo install tracedecay --no-default-features
```

Prebuilt Linux, macOS, and Windows archives are available on the [latest release](https://github.com/ScriptedAlchemy/tracedecay/releases/latest).

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
tracedecay init [path]              # enroll a project and publish its first generation
tracedecay sync [path]              # explicit administrative refresh
tracedecay sync --force [path]      # explicit full generation refresh
tracedecay status [path]            # graph stats, freshness, savings, cost
tracedecay tool                     # list every MCP tool
tracedecay tool search "<query>"    # CLI symbol search
tracedecay tool files               # indexed files
tracedecay tool affected --args -   # impacted tests/files ({"files":[...]})
tracedecay serve                    # MCP server
tracedecay doctor [--agent NAME]    # installation health check
tracedecay dashboard [--open]       # local dashboard
tracedecay monitor                  # live MCP savings/cost TUI
tracedecay update                   # refresh binary, plugins, daemon
tracedecay upgrade                  # self-upgrade current channel
```

Every MCP tool is reachable from the shell as `tracedecay tool <name>`; there
are no separate per-tool subcommands. Run `tracedecay tool` for the grouped
list and `tracedecay tool <name> --help` for one tool's parameters.

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

The daemon owns freshness and background convergence. Hooks, MCP, LSP, and
workspace events submit bounded, content-free hints; the daemon resolves exact
repository/worktree/ref/commit identity, captures a snapshot, and publishes an
immutable generation. Queries report the generation and typed
`warming`/`refresh_required`/`partial`/`unavailable` coverage while a newer
generation is being prepared; they never run a hidden sync or silently use an
ancestor. An explicit `tracedecay sync` remains an administrative refresh
request for diagnostics or offline workflows.

Linked git worktrees share one registered project authority through the
repository common directory. Initialize once from any checkout; do not copy a
TraceDecay store into another worktree. Queries and comparisons select exact
worktree/ref/commit generations and report typed stale, indexing, partial, or
unavailable state when that snapshot is not ready. Branches and worktrees do
not create separate databases or fact stores.

See [docs/BRANCHING-USER-GUIDE.md](docs/BRANCHING-USER-GUIDE.md) for branch
selection, provenance, and recovery behavior.

## Dashboard

```bash
tracedecay dashboard
tracedecay dashboard --port 8080
tracedecay dashboard --port 0 --open
```

The dashboard includes graph exploration, project memory, LCM session search, token savings, and cost views. See [docs/dashboard.md](docs/dashboard.md).

## Privacy

Core indexing, graph queries, MCP tools, memory, and dashboard data use the
local daemon-owned authority by default. That default does not mean every
command is offline: configured remote sources and authorities can exchange only
authorized, sanitized, classified records under their remote policy.

Network-capable effects are separate from core retrieval:

- Worldwide counter: when opted in, sends the pending aggregate token-savings
  amount and may fetch the public total/country display data.
- GitHub release checks and updates: contact GitHub release endpoints. An
  explicitly configured private GitHub review source can use a read-only
  credential from the operating-system keyring; missing, ambiguous,
  write-capable, or unverifiable credentials fail closed.
- Cost pricing refresh: `tracedecay cost` may retrieve public model-pricing
  data and cache it locally.
- Semantic models: when semantic auto-download is enabled, TraceDecay can
  download missing revision-pinned artifacts from Hugging Face hosts and verify
  their catalog-pinned lengths and SHA-256 digests before publication.
- Configured remote authority: an authenticated, policy-authorized peer can
  perform remote retrieval, replication, backup, restore, or failover. This
  path fails closed with typed `unavailable`, `denied`, or `stale-peer` state.

These services receive normal transport metadata, including the connection's
source address, in addition to any request payload. The counter request is an
aggregate amount, not repository content, but it is not an anonymity guarantee.
Use a network policy or firewall if a command must make no outbound connection.
An opt-out, denial, timeout, or unavailable remote service must remain visible
as that state; it is not evidence of a zero counter, an up-to-date release, or
current pricing. See [Security](SECURITY.md#network-access) for the complete
outbound-access and credential boundary.

Disable the worldwide counter with:

```bash
tracedecay disable-upload-counter
```

## Troubleshooting

```bash
tracedecay doctor
tracedecay status --json
```

`tracedecay doctor` is read-only installation and authority diagnostics. It
does not repair stores or rewrite host files; authorized maintenance is a
separate daemon operation with its own preview, receipt, and recovery state.

Common fixes:

- Not initialized: run `tracedecay init` from the project root.
- Agent does not see tools: run `tracedecay doctor --agent <name>`, then restart the agent.
- Missing symbols: inspect `tracedecay status --json` for the selected
  generation and typed warming/refresh-required coverage; request an explicit
  administrative refresh only when the daemon says it is needed, then confirm
  the file is not ignored.
- Slow first index: use daemon status/coverage to inspect warming and backlog;
  routine updates are daemon-owned.

## Build

Building from a source checkout requires Node.js 22+ and npm in addition to
Rust: `dashboard/app-dist/` is generated output and is not committed, so
`build.rs` runs `npm ci` and `npm run build` in `dashboard/` before embedding
the UI. Published crates ship a prebuilt `app-dist`, so `cargo install
tracedecay` needs no Node toolchain.

```bash
cargo build --release
cargo build --release --features medium
cargo build --release --no-default-features

cargo nextest run --workspace --all-features --no-fail-fast
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
