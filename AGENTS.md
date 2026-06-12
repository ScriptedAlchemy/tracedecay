## Learned User Preferences

- User prefers fresh, tool-backed verification over trusting agent or subagent reports — when work is claimed done, verify it yourself (run tools, inspect DBs/logs/UI) and confirm per project where applicable.
- User wants repo-native project tooling used for codebase review, planning, and durable decision capture when available.
- User prefers local checkout tooling over global installs during active development, and refreshes the global binary from the local checkout (`cargo install --path . --force`) after significant changes.
- For unshipped PR branch work, replace in-progress designs directly rather than adding compatibility shims for old branch-only behavior.
- When the user asks to remember preferences or decisions, persist concise durable facts using the project memory system when available.
- For large efforts, user wants fleets of concurrent multi-model subagents with strict per-agent file ownership so writers never collide; choose subagent models by task complexity.
- Commit only when explicitly asked, scope commits to the agent's own changes grouped by logical subsystem, and push only when told.
- Reported metrics (token savings, costs) must be honest and audited — net rather than gross math, cross-checked against real transcript/usage data.
- Fix flaky tests instead of skipping them; never skip tests to get CI green.
- Keep one-off migration scripts untracked and delete them once their results are verified per project.

## Learned Workspace Facts

- Memory deletion is permanent by design: no archive/soft-delete/restore features anywhere; dashboard curation hard-deletes facts.
- In Hermes, tracedecay is both the memory provider and the context-engine provider for every profile; the provider name is "tracedecay" (not "lcm"), replacing the legacy hermes-lcm and holographic_plus plugins.
- Hermes profiles bound to a project use that project's repo-level `.tracedecay` databases (legacy `.tokensave` directories are still honored as a fallback); only the default profile stores tracedecay data at the profile level under `~/.hermes`.
- The Hermes tracedecay plugin must keep working against stock, uncustomized Hermes — verified by a CI job that installs stock Hermes; the user's Hermes fork only adds optional extras.
- The canonical repo is the ScriptedAlchemy/tokensave fork until the GitHub repository is renamed: never push or open PRs to the aovestdipaperino upstream; only the tokensave-large-treesitters dependency intentionally stays pointed at upstream.
- The standalone `tracedecay dashboard` server is the canonical dashboard implementation; the Hermes plugin wraps and reuses it, layering Hermes-only extras (e.g. LLM-based curation) on top.
- `tracedecay install --local` scopes the database to the repo's `.tracedecay/`; otherwise storage lives at the user/profile level.

## Workspace Guidance

- Keep persistent guidance general and durable; avoid recording transient branch state, temporary schema numbers, or moment-in-time tool status here.
- Store detailed implementation decisions in the project memory system or PR docs instead of expanding this file with narrow session notes.

## Prefer tracedecay MCP tools

Before reading source files or scanning the codebase, use the tracedecay MCP tools (`tracedecay_context`, `tracedecay_search`, `tracedecay_callers`, `tracedecay_callees`, `tracedecay_impact`, `tracedecay_node`, `tracedecay_files`, `tracedecay_affected`). They provide instant semantic results from a pre-built knowledge graph and are faster than file reads.

If a code analysis question cannot be fully answered by tracedecay MCP tools, try querying the SQLite database directly at `.tracedecay/tracedecay.db` (tables: `nodes`, `edges`, `files`). Use SQL to answer complex structural queries that go beyond what the built-in tools expose.

If you discover a gap where an extractor, schema, or tracedecay tool could be improved to answer a question natively, propose to the user that they open an issue at https://github.com/ScriptedAlchemy/tokensave describing the limitation. **Remind the user to strip any sensitive or proprietary code from the bug description before submitting.**
