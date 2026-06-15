# TraceDecay for Codex

This plugin exposes the TraceDecay MCP server and Codex skills for code graph,
impact, recall, and context-saving workflows.

Codex command hooks are installed separately through `~/.codex/hooks.json`
because the current Codex plugin manifest schema supports MCP servers and
skills, but not plugin-declared hooks.
