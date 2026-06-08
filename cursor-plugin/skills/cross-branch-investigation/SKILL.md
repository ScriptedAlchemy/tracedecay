---
name: cross-branch-investigation
description: Search and compare code across git branches without switching your checkout, using tokensave's per-branch graphs. Use for "does symbol X exist on main", "what changed between branch A and B", cross-branch diff/search, or checking another branch's API before you depend on it.
---

# Cross-branch investigation

tokensave can keep a separate code graph per git branch, so you can query another branch's graph directly. These tools are read-only and never change your checkout.

## Workflow

1. **See what's tracked first → `tokensave_branch_list`** (no args): each tracked branch with DB size, parent, last-sync time, and which is current/default. If the list is empty or your target branch is missing, cross-branch data for that branch does not exist yet — see Enablement.
2. **Search another branch → `tokensave_branch_search`** (`branch` required, `query` required, `limit?`): find a symbol in that branch's graph (e.g. confirm `parse_config` exists on `main` before calling it).
3. **Compare two branches → `tokensave_branch_diff`** (`base?`, `head?`, `file?`, `kind?`): symbols `added` / `removed` / `changed` (signature differs). `base` defaults to the project default branch and `head` to the current branch, so a bare call compares current vs default. Narrow large diffs with `file` or `kind` (e.g. `kind: "function"`).

## Enablement

- Multi-branch tracking is **opt-in per branch** from the terminal: `tokensave branch add <branch>` (no MCP tool does this). The plugin's Cursor hooks already auto-track the **current** branch on `git checkout`/`switch`/`worktree add` and on workspace open, so branches you visit get tracked over time.
- To diff/search a branch you have **not** visited, ask the user to run `tokensave branch add <branch>` (or `git checkout <branch>` once) in the terminal, then retry. There is no env var / `mcp.json` flag for this.
- If a tool response is prefixed with a branch-fallback `WARNING`, the current branch isn't tracked and results came from the nearest ancestor — surface that to the user.

## Guardrails

- All three tools are read-only and parallel-safe; they open the target branch's DB without touching your working tree. Suggest `tokensave branch add` as a terminal step — never try to enable tracking via edit tools.

## Output

- The cross-branch search hits or the added/removed/changed symbol lists, with the branches compared and any fallback warning surfaced.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
