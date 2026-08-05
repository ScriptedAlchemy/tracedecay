# Quick Setup

## 1. Install

**Homebrew (macOS):**

```bash
brew install ScriptedAlchemy/tap/tracedecay
```

**Cargo (any platform):**

```bash
cargo install tracedecay
```

Verify it works:

```bash
tracedecay --help
```

## 2. Configure Claude Code

```bash
tracedecay claude-install
```

This single command configures everything — MCP server, tool permissions, PreToolUse hook, and CLAUDE.md rules. No scripts, no `jq`, works on macOS/Linux/Windows. Safe to re-run after upgrading.

## 3. Index your project

```bash
cd /path/to/your/project
tracedecay init
```

This enrolls the repository with the daemon-owned project store and indexes all
supported files. Store locations are daemon-owned implementation details:
clients, host integrations, and operators must not open, copy, edit, or query a
TraceDecay database directly. The default `full` feature set covers 50+
languages; `Cargo.toml` is the source of truth for exact membership of the
`lite` / `medium` / `full` tiers.

Check what was indexed:

```bash
tracedecay status
```

## 4. Use it with Claude

Once configured, Claude has access to these tools:

| Tool | What it does |
|------|-------------|
| `tracedecay_search` | Find symbols by name or keyword |
| `tracedecay_context` | Build AI-ready context for a task description |
| `tracedecay_callers` | Find all callers of a function |
| `tracedecay_callees` | Find all callees of a function |
| `tracedecay_impact` | Compute the impact radius of a symbol |
| `tracedecay_node` | Get detailed info about a specific symbol |
| `tracedecay_files` | List indexed project files with filtering |
| `tracedecay_affected` | Find test files affected by source changes |
| `tracedecay_status` | Show graph statistics and global tokens saved |
| `tracedecay_rank` | Rank nodes by relationship count (most implemented interface, etc.) |
| `tracedecay_largest` | Rank nodes by size — largest classes, longest methods |
| `tracedecay_complexity` | Rank functions by composite complexity score |
| `tracedecay_recursion` | Detect recursive call cycles |
| `tracedecay_doc_coverage` | Find public symbols missing documentation |
| `tracedecay_god_class` | Find classes with the most members |
| `tracedecay_coupling` | Rank files by fan-in/fan-out coupling |

Plus more typed operations — see [README.md](README.md) for the full list.

Claude will use these tools automatically when you ask questions about your codebase. Examples:

- *"How does the authentication module work?"* — uses `tracedecay_context`
- *"What calls the `processPayment` function?"* — uses `tracedecay_callers`
- *"If I change `UserService`, what else is affected?"* — uses `tracedecay_impact`
- *"Which tests need to run after I changed db/connection.rs?"* — uses `tracedecay_affected`
- *"What's the most implemented interface?"* — uses `tracedecay_rank`
- *"Are there any god classes?"* — uses `tracedecay_god_class`
- *"Any recursive calls in the codebase?"* — uses `tracedecay_recursion`

### Claude Desktop (manual)

For Claude Desktop, add the MCP server to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "tracedecay": {
      "command": "tracedecay",
      "args": ["serve", "--path", "/path/to/your/project"]
    }
  }
}
```

Replace `/path/to/your/project` with the absolute path to your indexed project.

## Keeping the index fresh

No routine manual synchronization is required. Hooks, MCP, LSP, and workspace
events submit bounded change hints; the daemon captures the selected snapshot
and converges a new immutable generation in the background. Reads retain
truthful coverage while it works: `warming`, `refresh_required`, `partial`, and
`unavailable` are states to inspect, not empty successful answers.

Use `tracedecay status --json` to see the selected generation and coverage. An
explicit `tracedecay sync` is an administrative refresh request for a diagnostic
or offline workflow, not the normal post-edit command. Request it only when the
daemon reports it is needed; use `tracedecay sync --force` only when that
administrative workflow requires a complete refresh.

## Branches and linked worktrees

TraceDecay keeps one project authority for a repository. Branches and linked
worktrees do not create separate databases or fact stores. Each code result is
bound to the exact repository, worktree, ref, commit, and immutable generation
that produced it, while project facts, sessions, and LCM history remain
project-wide. The daemon receives bounded change signals from hooks/MCP and
publishes complete generations; it never silently serves an ancestor or the
currently active checkout when an explicit snapshot is requested.

See [docs/BRANCHING-USER-GUIDE.md](docs/BRANCHING-USER-GUIDE.md) for exact
selection, provenance, and recovery behavior.
