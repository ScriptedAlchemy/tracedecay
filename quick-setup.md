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

This creates a `.tracedecay/` directory and indexes all supported files (15 languages). After the initial index, `tracedecay sync` picks up only changed files. To force a full re-index, use `tracedecay sync --force`.

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

Plus dozens more — see [README.md](README.md) for the full list of 70+ tools.

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

After making code changes, sync the graph:

```bash
tracedecay sync
```

The MCP server reads from the database on each request, so it picks up synced changes without restarting.

## Multi-branch (optional)

If you work on multiple branches, TraceDecay can keep a separate graph per branch so switching never causes stale results:

```bash
tracedecay branch add          # track the current branch
```

This copies the nearest ancestor's database and syncs only the changed files. See [docs/BRANCHING-USER-GUIDE.md](docs/BRANCHING-USER-GUIDE.md) for the full guide.
