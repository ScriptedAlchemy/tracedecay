# TraceDecay User Guide

Thanks for downloading TraceDecay!

TraceDecay is a code intelligence tool that builds a semantic knowledge graph of your codebase. It gives AI coding agents (like Claude Code) instant, structured access to your code's symbols, relationships, and dependencies — so they spend fewer tokens scanning files and more time writing code.

Core indexing and retrieval run through the local daemon by default. Configured
remote sources and authorities are separate, policy-bound effects; see
[Privacy and Network](#privacy-and-network) before assuming an offline-only
deployment.

> **Final V2:** `tracedecay-graph-db` is the sole Grafeo boundary. Incompatible
> persisted data returns `ResetRequired` and requires explicit reset or
> recreation. Storage, scope, and lossless retrieval rules are in the [V2
> operating model](V2-OPERATING-MODEL.md).

---

## Table of Contents

1. [Installing TraceDecay](#installing-tracedecay)
2. [Your First Index](#your-first-index)
3. [Connecting to Your Agent](#connecting-to-your-agent)
4. [Exploring Your Codebase from the CLI](#exploring-your-codebase-from-the-cli)
5. [Keeping the Index Fresh](#keeping-the-index-fresh)
6. [The Embedded File Watcher](#the-embedded-file-watcher)
7. [Checking Your Setup with Doctor](#checking-your-setup-with-doctor)
8. [Finding Affected Tests](#finding-affected-tests)
9. [MCP Tools for AI Agents](#mcp-tools-for-ai-agents)
10. [Supported Languages](#supported-languages)
11. [Privacy and Network](#privacy-and-network)
12. [Updating TraceDecay](#updating-tracedecay)
13. [Configuration Files](#configuration-files)
14. [Troubleshooting](#troubleshooting)

---

## Installing TraceDecay

Pick whichever method suits your platform.

**Linux and Apple silicon macOS:**

```bash
curl -fsSL https://github.com/ScriptedAlchemy/tracedecay/releases/latest/download/install.sh | bash
```

**Windows:**

Download the x86_64 Windows archive from the
[latest release](https://github.com/ScriptedAlchemy/tracedecay/releases/latest),
extract `tracedecay.exe`, and place it on `PATH`.

**Prebuilt binaries:**

Download from the [latest release](https://github.com/ScriptedAlchemy/tracedecay/releases/latest) and place the binary somewhere on your `PATH`. Archives are available for macOS (Apple Silicon), Linux (x86_64 and ARM64), and Windows (x86_64).

---

## Your First Index

Navigate to any project directory and run:

```bash
cd /path/to/your/project
tracedecay init
```

TraceDecay enrolls the repository with the daemon, captures an exact checkout
snapshot, and publishes a validated code generation. Project facts, sessions,
and lossless LCM remain project-wide; code generations retain exact repository,
checkout, worktree, ref, commit/tree, snapshot, and generation provenance.
Storage is daemon-owned (an explicit local `.tracedecay/` install is only a
location choice), and clients never open a project database directly.

Once it finishes, run `tracedecay status` to see what was indexed:

```bash
tracedecay status
```

This prints an overview of your project: the number of files, symbols, edges (relationships between symbols), language distribution, and how many tokens the index has saved you so far. If you just want the summary line without the ASCII art, pass `--short`:

```bash
tracedecay status --short
```

For machine-readable output, use `--json`.

### Why `init` is explicit and refresh is daemon-owned

`tracedecay init` is the one-time enrollment and first-generation operation.
After enrollment, hooks, MCP, LSP, and the daemon's bounded freshness ladder
submit content-free hints. The daemon reconciles native Git state, captures the
selected worktree snapshot, and publishes a complete generation in the
background. Queries continue serving the last complete generation while a
refresh is warming and report typed `refresh_required`, `warming`, `partial`,
or `unavailable` coverage when appropriate.

Linked git worktrees do not need their own `tracedecay init`. They resolve to the
same registered project authority while each code generation retains exact
worktree/ref/commit/snapshot identity. Facts, sessions, and LCM remain owned by
that project authority; branch and worktree labels are provenance only.

An explicit `tracedecay sync` remains an administrative refresh request for a
diagnostic or offline workflow; it is not the normal post-edit product path and
never opens a store outside the daemon.

### Incremental convergence

The daemon reconciles only the bounded changed set from each hint and reuses
unchanged content-addressed artifacts when their complete identity matches.
Duplicate hints and no-op saves produce no new durable work. A failed or
cancelled refresh leaves the prior complete generation readable.

### Force re-index

If the daemon reports a compatible generation rebuild is needed, an operator may
request an explicit full refresh with `--force`:

```bash
tracedecay sync --force
```

### Default Skips

TraceDecay respects `.gitignore` by default and skips common generated, vendored, and cache directories such as `node_modules`, `vendor`, `dist`, `build`, `coverage`, `.next`, `.turbo`, `.cache`, virtualenvs, and `__pycache__`.

If there are additional directories you never want indexed for a run, pass `--skip-folder`:

```bash
tracedecay sync --skip-folder generated-fixtures  # explicit administrative refresh
```

### Seeing what changed

Use the daemon's status/coverage result to see the selected generation, exact
snapshot provenance, changed/reused counts, and warming/backlog state. Doctor
is a read-only health diagnostic; changes require a separate authorized daemon
operation with its own preview and receipt.

### Diagnosing slow daemon convergence

If status reports warming or a backlog, inspect the daemon's typed coverage
first. An operator may request explicit per-operation diagnostics with `--verbose`
(`-v`) when the daemon reports that an administrative refresh is appropriate:

```bash
tracedecay sync --verbose  # explicit administrative diagnostics
```

Example output:

```
  [verbose] scanned 10432 files in 2.3s
  [verbose] stat-checked 10432 files in 0.1s
  [verbose] changes: 3 new, 847 stat-changed, 0 removed, 9582 unchanged
  [verbose] hashed 850 files in 1.2s (0 read errors)
  [verbose] content check: 12 modified, 838 mtime-only
  [verbose] indexed 15 files (204 nodes, 189 edges) in 0.3s
  [verbose] resolved 39841 references in 0.5s
✔ sync done — 3 added, 12 modified, 0 removed in 4412ms
```

This also works with `--force` for full re-index diagnostics.

### Respecting .gitignore

By default, tracedecay respects your `.gitignore` rules and skips ignored files during indexing. You can check the current setting or toggle it:

```bash
tracedecay gitignore              # show current setting
tracedecay gitignore on           # enable (default)
tracedecay gitignore off          # disable — index everything
```

Don't forget to add `.tracedecay` to your `.gitignore` so enrollment markers do not get committed:

```bash
echo .tracedecay >> .gitignore
```

---

## Connecting to Your Agent

TraceDecay works as an MCP (Model Context Protocol) server. AI coding agents connect to it to query your codebase instead of scanning files directly. The `install` command sets everything up automatically.

### Claude Code

```bash
tracedecay install
```

Claude Code owns marketplace registration, enabled state, cache, hook trust,
and permissions. When activation is missing, TraceDecay stages verified source
and prints the native activation command without writing a lifecycle receipt.
After that host-native action, run the install again so TraceDecay can
atomically record the catalog component set. The plugin bundles the MCP server,
lifecycle hooks, subagents, skills, and slash commands. `tracedecay
update-plugin` refreshes receipt-owned source only through the same component
transaction. TraceDecay does not migrate or rewrite Claude's host config.
The installed hooks submit bounded native lifecycle envelopes only:
`SessionStart`, `Stop`, and saved-edit `PostToolUse`
(`Edit|MultiEdit|Write|NotebookEdit`). The daemon owns all later capture,
indexing, staleness checks, compaction, and advisory work; a hook never routes
tools, reads a store, or starts a model.

### Other agents

TraceDecay has receipt-backed profile-wide install lifecycles for these agents:

```bash
tracedecay install --agent claude      # Claude Code (default)
tracedecay install --agent opencode    # OpenCode
tracedecay install --agent codex       # OpenAI Codex CLI
tracedecay install --agent gemini      # Gemini CLI
tracedecay install --agent hermes      # Hermes Agent
tracedecay install --agent copilot     # GitHub Copilot CLI
tracedecay install --agent cursor      # Cursor
tracedecay install --agent kiro        # AWS Kiro
tracedecay install --agent kimi        # Kimi Code CLI
```

Other host integrations can be detected by `doctor`, but do not appear in the
installer until they have a canonical first-party component route.

Each installed agent gets the profile-wide configuration its host supports:
MCP registration or native plugin tools, with permissions where available.

- Hermes installs one native user plugin through Hermes' plugin API.
- Cursor installs a local plugin in `~/.cursor/plugins/local/tracedecay` that bundles MCP, hooks, and the tracedecay rule.
- Codex uses Codex's plugin source, marketplace, and installed-cache flow: TraceDecay stages the source bundle and marketplace entry, then `codex plugin add tracedecay@personal` installs Codex's cache from that source. The plugin owns MCP, hooks, and skills. Codex's profile-wide install does not write `~/.codex/AGENTS.md`, `~/.codex/hooks.json`, activation config, or cache entries.
- Kimi Code CLI stages its plugin source at `~/.tracedecay/host-bundle-stage/kimi/tracedecay`; run the printed `/plugins install <staged-path>` command in Kimi Code, then rerun TraceDecay so it can record the staged source. Kimi owns `~/.kimi-code/plugins/installed.json` and its managed/cache paths.

Hermes setup writes the single user integration to
`~/.hermes/plugins/tracedecay/` and enables it in `~/.hermes/config.yaml` under
`plugins.enabled`. `install`, `update-plugin`, `reinstall`, `doctor`, and
`uninstall` all target that same integration. Hermes may use its own home for
host-owned config, plugins, and transcripts, but named Hermes profiles,
project-local `.hermes` directories, and `HERMES_HOME` never select a
TraceDecay installation, store, or project identity.

The plugin registers one Hermes-native wrapper per tracedecay tool, adds a
lightweight `pre_llm_call` steering hook, registers a `/tracedecay_status` slash
command when the installed Hermes version supports plugin commands, and bundles
a `tracedecay:tracedecay` plugin skill. It also registers a `tracedecay` memory
provider (holographic facts via exact fact tools / `fact_feedback` /
`memory_status`) and a `tracedecay` context engine that compresses long
conversations through the daemon's session authority. Project-attached sessions
and lossless LCM are project-wide; untethered user sessions remain
profile-wide. The context engine exposes native
`lcm_grep`, `lcm_load_session`, `lcm_describe`, `lcm_expand`,
`lcm_expand_query`, `lcm_status`, and `lcm_doctor` tools (backed by the
`tracedecay_lcm_*` MCP tools) and uses the same daemon-routed session authority
as every other host. The wrappers call
`tracedecay tool <name> --json --args <json>` with a real project root from the
host context or working directory when available, with a 600-second timeout
and truncated stdout/stderr in error JSON. Hermes configuration paths remain
host-owned inputs for plugin behavior; they never become TraceDecay storage
identities. Removed Hermes install flags (`--profile`, `--all-profiles`, and
`--project-root`) and removed MCP routing fields (`storage_scope` and
`hermes_home`) are errors, not compatibility aliases.

#### Verifying Hermes plugin and context-engine changes

When changing generated Hermes plugin or context-engine behavior, start with
TraceDecay's read-only analysis tools before rebuilding or reinstalling
anything: use `tracedecay_diff_context` to inspect modified symbols,
dependencies, and affected tests; `tracedecay_simplify_scan` for complexity,
duplication, dead-code, and coupling regressions; `tracedecay_test_risk` for
untested hot spots; `tracedecay_diagnostics` for structured compiler/type
feedback; and `tracedecay_run_affected_tests` for the focused test set when test
execution is appropriate.

For LCM/session issues, pair `tracedecay_lcm_status` with the read-only LCM
diagnostics (`tracedecay_lcm_doctor`, or the native Hermes `lcm_doctor` wrapper).
Inspect reported retention, payload, provenance, and coverage states.
Authorized retention and maintenance effects are separate daemon operations
with their own previews, confirmations, and receipts; the diagnostic path never
applies them.

Known Hermes API caveats: native `lcm_*` tool dispatch receives
`messages=messages`, but direct registered live-ingest tools should remain
gated unless the host explicitly forwards messages. The
`context_engine_tool_handlers_receive_messages` flag is a TraceDecay convention,
not stock Hermes API. Treat `compression.*` as built-in compressor config; only
`compression.enabled` gates auto-compaction globally.

Kiro setup registers the profile-wide `tracedecay` MCP server through
`kiro-cli`. It does not create steering files, custom agents, default-agent
settings, hooks, or workspace MCP registrations. See
[Kiro integration](KIRO-INTEGRATION.md) for the exact lifecycle.

The install is idempotent — safe to run again after upgrading tracedecay. You'll also be offered the option to set up an optional global git post-commit hint hook (more on that below).

### Profile-wide installs

Each install writes or stages the active profile's host integration; it does
not create per-repository host configuration. The host's workspace/session
context selects the active TraceDecay project at runtime.

Cursor install is plugin-based:

- `tracedecay install --agent cursor` installs `cursor-plugin/` into `~/.cursor/plugins/local/tracedecay`.
- The plugin MCP config runs `tracedecay serve --path ${workspaceFolder}`, so the server resolves the active workspace's project store instead of the plugin directory. If a host spawns the server without expanding `${workspaceFolder}`, `serve` warns and falls back to project discovery where possible (details in the plugin's `README.md`).
- Cursor install no longer writes `.cursor/mcp.json`, `.cursor/hooks.json`, `.cursor/rules/tracedecay.mdc`, or `.cursor/permissions.json`; approvals are left to Cursor approval/run-mode behavior.
- The plugin bundles Cursor-specific, fail-open hooks: `sessionStart`,
  `preCompact`, `afterFileEdit`, and `stop`. Each uses the native event only to
  make a bounded daemon-admission attempt. The daemon then owns transcript
  capture, indexing, compaction, branch/preflight work, and advisory delivery.
  Manual or external-terminal changes are still best covered by the git
  post-commit hook and on-demand MCP staleness checks.

Manual Cursor plugin install for local development:

```bash
mkdir -p ~/.cursor/plugins/local
ln -s /path/to/tracedecay/cursor-plugin ~/.cursor/plugins/local/tracedecay
```

Reload Cursor after installing or replacing the plugin. The plugin expects the `tracedecay` binary to be available on `PATH`; ensure your shell PATH resolves the intended installed binary.

Codex global install is plugin-based for MCP, hooks, and skills. TraceDecay
stages the plugin source bundle and marketplace entry; Codex CLI installs its
cache from that source. First install writes
`~/plugins/tracedecay/`, updates `~/.agents/plugins/marketplace.json`, and prints
`codex plugin add tracedecay@personal`. Run that command in Codex to copy the
source into `~/.codex/plugins/cache/personal/tracedecay/<version>`, then rerun
`tracedecay install --agent codex` to record the staged source. `tracedecay
update-plugin --agent codex` refreshes only the source and tells Codex to update
its own cache; it never rewrites Codex activation or cache state.

Skill visibility follows Codex's plugin model. `codex plugin list` and
`codex plugin add` inspect the marketplace source bundle. Active Codex sessions
load skills, MCP config, and bundled hooks from the installed cache, not directly
from `~/plugins/tracedecay`; start a new Codex session after adding the plugin or
recopying it. Codex also skips new or changed command hooks until you trust them,
so run `/hooks` inside Codex after install or recopy.

Current Codex limitations: TraceDecay can refresh the plugin source and
marketplace entry, but it cannot force Codex to run `plugin add`, update or
remove its cache, reload an active session, or trust plugin command hooks for
you. Remove the native plugin with `codex plugin remove tracedecay@personal`,
then rerun `tracedecay uninstall --agent codex` to clean its staged source. The
legacy Codex config surfaces are intentionally left alone.

Kimi's global lifecycle is also two-step: TraceDecay stages source, then Kimi
Code's `/plugins install <staged-path>` registers it. To remove it, use Kimi
Code's `/plugins remove tracedecay` first, then rerun `tracedecay uninstall
--agent kimi` to remove the staged source. TraceDecay never writes Kimi's
managed plugin directory or `installed.json`.

The generated MCP entries use the resolved absolute path to the current `tracedecay` executable.

#### Config backups

Whenever tracedecay rewrites an agent config file — on `install`, on `uninstall`,
or an explicitly authorized host-maintenance operation — it first copies the
original to a sibling `.bak` file in the same directory. Doctor only reports
configuration findings; it never rewrites hooks. For example:

- `~/.claude.json` → `~/.claude.json.bak`

If anything goes wrong (a typo, an unexpected rewrite, an unknown bug), restore with `cp <path>.bak <path>`. The `.bak` is always the **exact bytes** of whatever was on disk just before the write; tracedecay never deletes or rotates it, so the most recent backup is the file you want.

### Removing an integration

```bash
tracedecay uninstall                   # remove Claude Code integration
tracedecay uninstall --agent codex     # remove Codex integration
tracedecay uninstall --agent hermes
```

---

## Exploring Your Codebase from the CLI

You don't need an AI agent to use tracedecay. Every MCP tool is reachable from
the shell through `tracedecay tool <name>`, which dispatches the same tool the
agent would call. There are no separate per-tool subcommands — `tracedecay
query`, `tracedecay context`, `tracedecay files`, and `tracedecay affected` do
not exist and will fail with an unrecognized-subcommand error.

```bash
tracedecay tool                        # every tool, grouped
tracedecay tool search --help          # one tool's parameters
```

Tool names work with or without the `tracedecay_` prefix, and dashes and
underscores are interchangeable (`dead-code` == `dead_code`). `--json` prints
the raw payload instead of the human rendering.

### Searching for symbols

```bash
tracedecay tool search "authenticate"
```

This searches the index for symbols matching your query. It returns function names, class names, method names, and their file locations and signatures. Limit results with `--limit`:

```bash
tracedecay tool search "authenticate" --limit 5
```

### Building task context

```bash
tracedecay tool context "implement user authentication"
```

This is the same context builder that AI agents use. Given a natural language task description, it finds the most relevant entry points, related symbols, and code structure. Output defaults to the human text rendering; use `--json` for the raw payload.

```bash
tracedecay tool context "implement user authentication" --json --max-nodes 30
```

The `--max-nodes` flag controls how many symbols are included (default: 20).

### Listing indexed files

```bash
tracedecay tool files                           # all files
tracedecay tool files --path src/mcp            # only files under src/mcp/
tracedecay tool files --pattern "**/*.rs"       # only Rust files
tracedecay tool files --json                    # machine-readable output
```

### Running the MCP server directly

```bash
tracedecay serve
```

This starts the MCP server over stdio. You normally don't need to run this yourself — the agent integration handles it. But it's useful for debugging or connecting custom tools.

### Working from a subdirectory

You can open your AI agent from any subdirectory of an enrolled project.
TraceDecay resolves the registered project and exact worktree through the daemon;
it does not choose a database by walking to the nearest path.

When the MCP server starts from a subdirectory, listing tools like
`tracedecay_files`, `tracedecay_search`, and `tracedecay_context` automatically
scope their results to that subdirectory while retaining the project/worktree
identity resolved by the daemon. This is useful in monorepos or large projects
where you want to focus on one area.

Graph traversal tools (`tracedecay_callers`, `tracedecay_callees`, `tracedecay_impact`, etc.) remain unscoped so you can still follow connections across directory boundaries.

You can always override the automatic scope by passing an explicit `path` parameter to any tool. `tracedecay_status` shows the active scope prefix when one is in effect.

---

## Keeping the Index Fresh

The daemon owns freshness and convergence. Hooks, MCP, LSP, and workspace
events submit bounded, content-free hints; the daemon coalesces them, resolves
the exact repository/worktree/ref/commit state with native Git, and publishes a
validated generation. Exact, lexical, and graph queries remain available from
the last complete generation while semantic or newer work is warming.

Every freshness-sensitive result reports the generation/snapshot it used and
typed coverage such as `warming`, `refresh_required`, `partial`, or
`unavailable`. A backlog or unavailable daemon is visible state, not a reason
to silently use an ancestor branch or return an empty success.

### Host change hints

During `tracedecay install`, supported hosts can send post-edit, stop, commit,
or workspace hints to the daemon. Hints are non-blocking and contain no source
payload. They never open a TraceDecay database or run a branch tracking command.

You can also set it up manually:

**Global (all repos):**

```bash
git config --global core.hooksPath ~/.git-hooks
mkdir -p ~/.git-hooks
cp scripts/post-commit ~/.git-hooks/post-commit
chmod +x ~/.git-hooks/post-commit
```

**Per-repo:**

```bash
cp scripts/post-commit .git/hooks/post-commit
chmod +x .git/hooks/post-commit
```

## MCP freshness checks

MCP calls perform a bounded freshness check and report the selected generation
or a typed warming/refresh-required state. They do not run an implicit refresh
or open storage. Hooks and the daemon scheduler own background convergence;
multiple clients are serialized by the daemon authority.

### Optional daemon service

If you want the daemon available across terminal sessions and after login, install the per-user service:

```bash
tracedecay daemon install-service
tracedecay daemon status
```

On Linux this installs a systemd user service. On macOS this installs a LaunchAgent at `~/Library/LaunchAgents/com.tracedecay.daemon.plist`. On Windows this registers a least-privilege, per-user Task Scheduler task that starts at logon. The task name and ACL are scoped to the current Windows SID, and the daemon endpoint is an authenticated loopback connection discovered from the selected profile.

Use `tracedecay daemon start`, `stop`, or `restart` for explicit lifecycle control. Remove the service with:

```bash
tracedecay daemon uninstall-service
```

### CLI-Only Workflows

If you don't keep an agent attached, install the supported daemon service and
optional Git hint hook:

```bash
cp scripts/post-commit .git/hooks/post-commit
chmod +x .git/hooks/post-commit
```

Use `tracedecay sync` only as an explicit administrative refresh request when a
diagnostic says the selected generation is stale; routine freshness remains a
daemon-owned background journey.

## Checking Your Setup with Doctor

The `doctor` command runs a read-only health check:

```bash
tracedecay doctor
```

It verifies:

- **Binary** — location and version
- **Current project** — registered project identity, final-store admission,
  exact worktree/ref/commit/generation, freshness, coverage, and typed authority
  state
- **Global registry** — daemon-owned project/profile enrollment and availability
- **User config** — `~/.tracedecay/config.toml` and upload settings
- **Agent integrations** — MCP server registration, hook installation, tool permissions, prompt rules
- **Network** — the configured worldwide counter and GitHub releases API; each
  reports its own available or unavailable state

If any tool permissions are missing after an upgrade, Doctor reports the missing
capability and the supported install/update operation. Doctor only reports
state; refresh, retention, recreation, and host-config changes are separate
authorized daemon operations.

To check only a specific agent:

```bash
tracedecay doctor
```

The accepted agent values are the same values supported by `tracedecay install --agent`.

---

## Finding Affected Tests

When you change source files, you often want to know which tests might be affected. The `affected` tool traces through the file dependency graph to find them. `files` is an array, so pass the whole arguments object with `--args`:

```bash
tracedecay tool affected --args '{"files":["src/main.rs","src/db/connection.rs"]}'
```

This performs a breadth-first search from the changed files through import/dependency edges to find test files that directly or transitively depend on those files.

### Piping from git

This is especially useful in CI pipelines. `--args -` reads the arguments
object from stdin, so build it from `git diff`:

```bash
git diff --name-only HEAD~1 \
  | jq -R -s -c '{files: (split("\n") | map(select(length > 0)))}' \
  | tracedecay tool affected --args -
```

There is no `--stdin` flag; the file list travels inside the arguments object.

### Options

```bash
# limit traversal depth (default: 5)
tracedecay tool affected --args '{"files":["src/lib.rs"],"depth":3}'

# custom test file pattern
tracedecay tool affected --args '{"files":["src/lib.rs"],"filter":"*_test.rs"}'

# raw JSON payload instead of the human rendering
tracedecay tool affected --args '{"files":["src/lib.rs"]}' --json
```

---

## MCP Tools for AI Agents

When running as an MCP server, tracedecay exposes typed operations that AI agents can call. Here's what they do, grouped by purpose.

### Core exploration

| Tool | What it does |
|------|-------------|
| `tracedecay_context` | Given a task description, returns relevant symbols, relationships, and code snippets. This is the go-to starting point for any coding task. |
| `tracedecay_grep` | Search indexed code content by literal string or regex, with each hit annotated by its enclosing symbol. |
| `tracedecay_search` | Find symbols by name. Supports filtering by kind (function, class, method, etc.). |
| `tracedecay_node` | Get full details for a specific symbol: source code, location, complexity metrics, and relationships. |
| `tracedecay_files` | List indexed files, optionally filtered by directory or glob pattern. |
| `tracedecay_status` | Index statistics: file counts, symbol counts, language distribution, and tokens saved. |

### Navigating relationships

| Tool | What it does |
|------|-------------|
| `tracedecay_callers` | Find what calls a given function or method. Configurable traversal depth. |
| `tracedecay_callees` | Find what a function or method calls. |
| `tracedecay_impact` | Trace the full blast radius of changing a symbol — everything that could be affected. |
| `tracedecay_affected` | Find test files affected by source file changes. |
| `tracedecay_similar` | Find symbols with similar names (useful for naming patterns or related code). |
| `tracedecay_rename_preview` | Preview all references to a symbol before renaming it. |

### Code quality analysis

| Tool | What it does |
|------|-------------|
| `tracedecay_dead_code` | Find unreachable symbols — functions with no callers. |
| `tracedecay_unused_imports` | Find import statements that are never referenced. |
| `tracedecay_circular` | Detect circular file dependencies. |
| `tracedecay_recursion` | Detect recursive and mutually-recursive call cycles. |
| `tracedecay_complexity` | Rank functions by composite complexity score, including cyclomatic complexity from the AST. |
| `tracedecay_god_class` | Find classes with the most members — candidates for decomposition. |
| `tracedecay_hotspots` | Find the most connected symbols (highest call count). These are high-risk areas. |
| `tracedecay_doc_coverage` | Find public symbols missing documentation. |
| `tracedecay_simplify_scan` | Quality analysis of changed files: duplications, dead code, complexity, coupling. |

### Health & quality signals

| Tool | What it does |
|------|-------------|
| `tracedecay_health` | Composite quality signal (0–10000) from five structural dimensions (acyclicity, depth, equality, redundancy, modularity) with a low-weight penalty for `/// skip-test-coverage` overuse. The single number to track over time. |
| `tracedecay_gini` | Gini inequality coefficient for any metric (complexity, lines, fan-in, fan-out, members). Finds god files and uneven distributions. |
| `tracedecay_dependency_depth` | Longest file-level dependency chains — the critical paths where upstream changes ripple through the most layers. |
| `tracedecay_dsm` | Design Structure Matrix showing file dependencies as clusters, density stats, or an NxN grid. Reveals hidden coupling patterns. |
| `tracedecay_test_risk` | Risk-weighted test gaps combining complexity, coupling, git churn, and test coverage. Answers "where should the next test go?" Reports a **static attribution lower bound** (not line/branch coverage): each function is attributed via a direct test edge (`direct_unit`) or a depth-3 transitive path (`closure`), with the weaker `closure` method keeping a higher residual risk. See [Reading the test_risk / test_map coverage signal](./TEST-MAP-INTERPRETATION.md) for how to interpret the signal honestly on integration-heavy repos. |

### Test Coverage Conventions

#### `/// skip-test-coverage`

Mark functions that are genuinely untestable in unit tests (e.g. infrastructure-dependent, framework-invoked, or private helpers tested only transitively):

```rust
/// skip-test-coverage
pub async fn produce(&mut self, topic: &str, batch: Bytes) -> io::Result<i64> { ... }
```

Marked functions are excluded from `tracedecay_test_risk` attribution calculations, giving you an accurate picture of testable-code attribution (the `skipped` count appears in the summary). Note this is a **static attribution** signal, not executed coverage — see [Reading the test_risk / test_map coverage signal](./TEST-MAP-INTERPRETATION.md).

**Health penalty:** The `coverage_discipline` dimension (visible in `tracedecay_health` and `tracedecay_health_delta`) penalises overuse. Each skipped function lowers the score proportionally — a few genuine exclusions have negligible impact, but marking 50%+ of your codebase as untestable will visibly reduce your quality signal. This encourages using the annotation for its intended purpose rather than as a way to game coverage numbers.

### Structural analysis

| Tool | What it does |
|------|-------------|
| `tracedecay_module_api` | Public API surface of a file or directory. |
| `tracedecay_coupling` | Rank files by coupling (fan-in or fan-out). |
| `tracedecay_inheritance_depth` | Find the deepest class inheritance hierarchies. |
| `tracedecay_type_hierarchy` | Recursive type hierarchy tree for traits, interfaces, and classes. |
| `tracedecay_distribution` | Node kind breakdown (classes, methods, fields) per file or directory. |
| `tracedecay_rank` | Rank nodes by relationship count (most-implemented interface, most-extended class, etc.). |
| `tracedecay_largest` | Rank nodes by size — largest classes, longest methods. |

### Git-aware tools

| Tool | What it does |
|------|-------------|
| `tracedecay_diff_context` | Semantic context for changed files: modified symbols, dependencies, and affected tests. |
| `tracedecay_changelog` | Semantic diff between two git refs — which symbols were added, removed, or modified. |
| `tracedecay_commit_context` | Semantic summary of uncommitted changes, useful for drafting commit messages. |
| `tracedecay_pr_context` | Semantic diff between git refs for pull request descriptions. |
| `tracedecay_test_map` | Source-to-test mapping at the symbol level, with uncovered symbol detection. Finds test callers up to depth 3, so a listed test may be a direct caller or a transitive one — see [Reading the test_risk / test_map coverage signal](./TEST-MAP-INTERPRETATION.md) for the direct-vs-closure distinction. |

### Porting tools

| Tool | What it does |
|------|-------------|
| `tracedecay_port_status` | Compare symbols between source/target directories to track cross-language porting progress. |
| `tracedecay_port_order` | Topological sort of symbols for porting — tells you what to port first based on dependencies. |

### Memory and fact recall

The holographic memory tools store durable facts linked to entities:

| Tool | What it does |
|------|--------------|
| Exact `tracedecay_fact_store_*` tools | Store, search, update, remove, and reason over facts linked to entities such as symbols, files, branches, subsystems, people, or concepts. |
| `tracedecay_fact_feedback` | Record `helpful` or `unhelpful` feedback for a numeric `fact_id` so the fact's computed trust score changes over time. |
| `tracedecay_memory_status` | Read-only report of project/profile fact and entity counts, trust-score buckets, feedback counts, coverage, and missing-vector state. It never repairs or mutates storage. |

Entity recall surfaces facts by named entity and includes why each fact was recalled: matching entities, reason text, related fact IDs, contradiction links, and the current trust score. Update old prompts and permissions to use the exact `tracedecay_fact_store_*` tools, `tracedecay_fact_feedback`, and `tracedecay_memory_status`.

Common exact fact-tool payloads (in add/search/probe order):

```json
{"content": "Repository uses profile-wide host installs during active development.", "entities": ["install", "tracedecay"], "category": "project", "source": "user", "tags": ["preference"], "trust": 0.9}
{"query": "profile-wide host install preference", "min_trust": 0.5, "limit": 10}
{"entity": "tracedecay"}
```

Common `tracedecay_fact_feedback` payloads:

```json
{"fact_id": 42, "action": "helpful", "source": "agent", "note": "Matched the current code path."}
{"fact_id": "42", "unhelpful": true, "source": "user", "note": "Superseded by a newer decision."}
```

For exact fields, inspect the live MCP descriptors; the generated schemas are the source of truth.

Discovery and analysis tools are read-only and safe to call in parallel. Session
baseline, memory, and feedback mutations route through the daemon and return
typed receipts; they never write host sidecars or open a project database
directly. Edit tools modify source files.

---

## Supported Languages

TraceDecay supports more than 50 languages, organized into three tiers. Each tier includes all the languages from the tier below it.

### Lite (14 languages)

Always compiled. The smallest binary for the most popular languages.

Rust, Go, Java, Scala, TypeScript, JavaScript, Python, C, C++, Kotlin, C#, Swift, Svelte, Astro

### Medium (Lite + 9 = 23 languages)

Adds scripting, config, and additional systems languages.

Dart, Pascal, PHP, Ruby, Bash, Protobuf, PowerShell, Nix, VB.NET

### Full (Medium + 27+ languages)

Everything, including legacy and niche languages.

Lua, Zig, Objective-C, Perl, Batch/CMD, Fortran, COBOL, MS BASIC 2.0, GW-BASIC, QBasic, QuickBASIC 4.5

### Mixing individual languages

Source builds can cherry-pick individual languages without taking a full tier:

```bash
cargo build --release --no-default-features --features lang-nix,lang-bash
```

### What gets extracted

For each supported language, tracedecay extracts:

- Function and method definitions (with signatures)
- Class, struct, trait, interface, and enum definitions
- Fields and properties
- Import and export statements
- Call relationships and type references
- Docstrings and annotations
- Complexity metrics (branches, loops, returns, max nesting, cyclomatic complexity)
- Cross-file dependency edges

---

## Privacy and Network

TraceDecay's core functionality is local-first. Indexing, search, graph queries,
and the MCP server run through the local daemon and its embedded Grafeo/SQLite
authority. Clients do not open database files directly. Default local and
public-repository behavior needs no credential, but configured remote sources
and authorities are distinct policy-bound effects.

Network effects are separate from local indexing and retrieval. They can be
disabled, unavailable, or denied without turning those states into successful
local results. The available effects are described below.

### Worldwide token counter

TraceDecay tracks how many tokens it has saved locally. If you opt in, the
daemon's token-savings status path uploads that aggregate count to the worldwide
counter. Repository content, file names, and project names are not part of the
counter payload. The counter service still receives ordinary transport metadata
such as the source IP and may derive aggregate geography from it; submitting an
aggregate count is not an anonymity guarantee.

This powers the "Worldwide" counter shown in `tracedecay status` only when the counter is enabled.

**To opt in:**

```bash
tracedecay enable-upload-counter
```

Disable it again at any time:

```bash
tracedecay disable-upload-counter
```

### Version check

TraceDecay checks GitHub release endpoints to show an upgrade notice. GitHub
receives ordinary request metadata, including the connection source address and
the TraceDecay user agent. A timeout or unavailable service means release
metadata is unavailable, not that no update exists.

### Private GitHub review sources

An explicitly configured private GitHub review source can use an optional
read-only credential from the operating-system keyring. Configuration stores a
keyring locator rather than the secret itself. The daemon mounts the source only
after verifying the exact read-only permission set; missing, ambiguous,
write-capable, or unverifiable credentials fail closed.

### Provider usage and pricing

TraceDecay records provider usage as immutable observations from exact native
evidence. Each observation retains the provider/model identity, native scope and
counter semantics, native field/kind, source range, and any native correlation
identifiers. A read never infers missing identity or counters from a neighboring
message, and cumulative-to-delta derivation remains deterministic and
issue-marked.

`tracedecay cost` is a side-effect-free read over those observations. It uses one
deterministic bundled all-provider pricing table, identified by its content
digest. It does not make a request-triggered pricing fetch, write a home-directory
pricing cache, or consult a pricing environment override. Missing native usage,
unknown models, unavailable observations, or unavailable pricing remain typed
unknown/unavailable results; TraceDecay never fills them with zero or a stale
fallback estimate.

### Semantic-model acquisition

When semantic auto-download is enabled, TraceDecay can download missing,
revision-pinned semantic-model artifacts from Hugging Face hosts. Artifacts are
verified against catalog-pinned lengths and SHA-256 digests before publication.
If acquisition is unavailable or disabled, semantic retrieval reports its model
state or failure while exact, lexical, and graph retrieval remain available.

### Configured remote authority

An explicitly configured, authenticated remote authority can perform
policy-authorized remote retrieval, replication, backup, restore, or failover.
Only authorized, sanitized, classified records can be exchanged, with exact
source, retention, coverage, and receipt identity. The remote path fails closed
with typed `unavailable`, `denied`, or `stale-peer` state; it does not turn a
host, transport, or arbitrary endpoint into a TraceDecay storage authority.

See [Security](../SECURITY.md#network-access) for the complete outbound-access,
credential, and local-listener boundary.

---

## Updating TraceDecay

When a new version is available, tracedecay tells you during `status` (or an
explicit administrative refresh):

```
Update available: v3.3.3 -> v3.4.0
  Run: tracedecay upgrade
```

The `upgrade` command downloads the latest release from GitHub and replaces the binary in place:

```bash
tracedecay upgrade
```

Beta and stable are separate update channels — a beta build only sees beta releases and vice versa. Any attached MCP servers will continue running with the previous binary until you restart your agent.

After upgrading, re-run install if the host integration reports a missing
capability, then inspect the daemon-owned status/coverage:

```bash
tracedecay install
tracedecay doctor
tracedecay status --json
```

If the status is `reset_required`, stop reads and writes for the affected
authority and follow the daemon's typed remediation or reset instructions. Do
not copy or edit database files, bypass the daemon, or reopen the authority
until remediation completes or the daemon explicitly recreates the final store.

---

## Configuration Files

TraceDecay stores data through one daemon-owned project/profile authority.
Clients and hosts never open the underlying files directly.

### User memory store

The profile-owned user-memory store stores durable user preferences and memory
from chat sessions that are not attached to an initialized TraceDecay project. Use
`memory_scope=user` with exact `tracedecay_fact_store_*` tools,
`tracedecay_fact_feedback`, or `tracedecay_memory_status`. The CLI can access
this scope outside any project. Hermes routes untethered chat and explicit
user-preference writes here; projectless Codex and Cursor hooks recall from it.

### Active project store

Projects enroll one daemon-owned project authority. An explicit local
`.tracedecay/` directory is only a location choice; profile-backed storage may
keep the final Grafeo/SQLite stores under a private profile root, with an
enrollment marker in the repository. Project facts, sessions, and lossless LCM
are project-wide across branches and linked worktrees. Code graphs are indexed
as immutable generations with exact repository, checkout, worktree, ref,
commit/tree, snapshot, and generation provenance.

The user-level registry database records enrollment and routing metadata; it is
not a fact authority. Retention, compaction, payload quarantine, and rebuilds
are separate daemon operations with receipts. Hosts and clients never become a
storage authority or open a database directly.

Add the explicit local enrollment marker to your `.gitignore` if your setup uses
one, but do not copy or edit store files.

An incompatible persisted shape or incomplete privacy remediation returns
`ResetRequired`/`reset_required`. Follow the daemon's remediation or explicitly
recreate the final store; runtime never guesses, falls back, or exposes
unverified content.

### Cross-project reads

Most commands still default to the active project discovered from your current directory. For intentional cross-project reads, run commands from the target checkout or use the path selectors supported by each command:

```bash
tracedecay status /path/to/project --json
tracedecay memory status --path /path/to/project --json
```

`tracedecay sessions search` searches previously ingested sessions for the active project. By default it searches all ingested transcript providers; pass `--provider <id>` only when intentionally constraining the search. Use `--project-id` or `--project-path` to search a registered project other than the current directory.

### Per-user: `~/.tracedecay/`

Created in your home directory. Contains:

- `config.toml` — user preferences (upload opt-in/out, cached version info, pending upload count)
- `global.db` — daemon-owned registry/usage metadata for enrolled projects; it is
  not a fact authority and clients never open it directly
- `projects/<project_id>/` — daemon-owned project authority data when profile storage is enabled

The `config.toml` is plain TOML and fully transparent:

```toml
upload_enabled = false      # set to true to opt in to counter upload/read
pending_upload = 4823       # tokens waiting to be uploaded
last_upload_at = 1711375200 # last successful upload timestamp
last_worldwide_total = 1000000
last_worldwide_fetch_at = 1711375200
```

#### Mandatory structured sanitization of ingested transcripts

Agent transcripts can contain credentials or other sensitive values. TraceDecay
applies one canonical, structured sanitizer to every ingest, replay, and
derived-content path before content becomes durable or searchable. It parses
JSON and other structured values before scanning, redacts values whose field
meaning or credential evidence is sensitive, preserves valid document shape,
and binds a `SanitizationReceiptV1` to the source and sanitized content.

Sanitization is mandatory. It cannot be disabled, narrowed, or overridden by a
profile setting, host configuration, or message metadata. LCM payloads are
never retained verbatim: clean content is accepted, detected secrets are
replaced and marked redacted/lossy, and malformed, oversized, unverifiable, or
sanitizer-failing content is quarantined or rejected fail-closed. Externalized
payloads are sanitized before storage and are represented in projections by a
safe placeholder.

The daemon's LCM status reports scan, quarantine, derivative-rebuild, and
reset-required phases. Reads remain locked while remediation is incomplete;
the daemon sanitizes recoverable inline rows, quarantines content it cannot
prove safe, rebuilds derivatives atomically, and requires explicit reset when
the retained payload or privacy revision cannot be verified. Follow the typed
status and reset instructions; never copy or edit store files or hand-edit
sanitization metadata.

---

## Troubleshooting

### "tracedecay not initialized"

TraceDecay could not find an initialized project store for your current directory. Run:

```bash
tracedecay init
```

### MCP server not connecting

Your AI agent doesn't see tracedecay tools.

1. Run `tracedecay doctor` to check the integration
2. Verify `tracedecay` is on your PATH: `which tracedecay`
3. Re-run `tracedecay install` and restart your agent completely

### Missing symbols in search

Some symbols aren't showing up.

- Check `tracedecay status --json` for the selected generation and typed
  warming/refresh-required state. Request an explicit administrative refresh
  only when the daemon reports it is needed.
- Check that the language is supported (see the tiers above)
- Verify the file isn't being skipped by `.gitignore` (`tracedecay gitignore` to check)

### Indexing is slow on first run

The initial generation of a large project can take a few seconds. This is
normal. Use daemon status/coverage to see warming progress and backlog state.

- Subsequent daemon reconciliations are incremental and much faster
- Routine updates are daemon-owned; do not build a client-side refresh loop into
  your day-to-day workflow.
- Post-commit and daemon hook notifications are bounded and fail open so a slow
  or unavailable daemon does not hold up agent work for long.

### Stale install warning

If you see a warning about your install being stale after an upgrade, run:

```bash
tracedecay install
```

This updates tool permissions, hooks, prompt rules, and plugin bundles where applicable to match the new version.

### Getting help

If you run into something not covered here, check the [GitHub repository](https://github.com/ScriptedAlchemy/tracedecay) or open an issue.
