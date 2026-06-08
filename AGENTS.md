## Learned User Preferences

- User prefers fresh, tool-backed verification for setup/configuration work and often asks agents to prove changes actually work.
- User wants repo-native project tooling used for codebase review, planning, and durable decision capture when available.
- User prefers local checkout tooling over global installs during active development, so tool behavior reflects the current branch.
- For unshipped PR branch work, replace in-progress designs directly rather than adding compatibility shims for old branch-only behavior.
- When the user asks to remember preferences or decisions, persist concise durable facts using the project memory system when available.

## Workspace Guidance

- Keep persistent guidance general and durable; avoid recording transient branch state, temporary schema numbers, or moment-in-time tool status here.
- Store detailed implementation decisions in the project memory system or PR docs instead of expanding this file with narrow session notes.

## Prefer tokensave MCP tools

Before reading source files or scanning the codebase, use the tokensave MCP tools (`tokensave_context`, `tokensave_search`, `tokensave_callers`, `tokensave_callees`, `tokensave_impact`, `tokensave_node`, `tokensave_files`, `tokensave_affected`). They provide instant semantic results from a pre-built knowledge graph and are faster than file reads.

If a code analysis question cannot be fully answered by tokensave MCP tools, try querying the SQLite database directly at `.tokensave/tokensave.db` (tables: `nodes`, `edges`, `files`). Use SQL to answer complex structural queries that go beyond what the built-in tools expose.

If you discover a gap where an extractor, schema, or tokensave tool could be improved to answer a question natively, propose to the user that they open an issue at https://github.com/ScriptedAlchemy/tokensave describing the limitation. **Remind the user to strip any sensitive or proprietary code from the bug description before submitting.**
