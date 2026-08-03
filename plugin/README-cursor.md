# TraceDecay Cursor Plugin

This bundle is installed by:

```bash
tracedecay install --agent cursor
```

Reload Cursor after installing or replacing the plugin. `tracedecay install
--agent cursor` writes a real plugin directory rather than a symlink and rewrites
MCP/hook commands to the resolved absolute `tracedecay` executable path so
GUI-launched Cursor does not depend on shell `PATH`.

The plugin registers the TraceDecay MCP server under the `tracedecay` key as:

```bash
tracedecay serve --path ${workspaceFolder}
```

Cursor Settings surfaces that MCP server key literally, so the Cursor bundle
uses `tracedecay` (not the Claude/Codex `graph` key). Each Cursor workspace
gets its own `.tracedecay/` index. Cursor's MCP runner resolves
`${workspaceFolder}` in normal editor windows.

Some Cursor contexts (headless agent-session MCP scopes) pass the literal,
unexpanded `${workspaceFolder}` from the user home directory. Cursor never
retries a failed MCP scope, so `serve` detects unexpanded `${...}` values,
warns on stderr, and falls back to project discovery: cwd walk-up, MCP
initialize roots, then the global project registry. Registry fallback accepts
only a unique registered project; otherwise `serve` exits with an actionable
"multiple projects" error. The template keeps `--path ${workspaceFolder}`
because normal Cursor windows expand it and home-dir discovery cannot scope
multi-project setups. If tools still do not connect, run
`tracedecay doctor --agent cursor`.

Hook commands derive the active project from Cursor's event payload /
`CURSOR_PROJECT_DIR`, not from the plugin directory.

Every MCP tool is also available from the shell as `tracedecay tool <name>`
(`tracedecay tool` lists tools; `tracedecay tool <name> --help` shows
parameters). The bundled `using-the-cli` skill and always-applied rule use
that CLI fallback when MCP transport errors or times out, instead of querying
`.tracedecay` databases.

## PR13 desktop native diagnostics

Cursor desktop installs the unpacked `tracedecay.cursor-native` VS Code
extension through TraceDecay's receipt-backed host-component lifecycle. Reload
Cursor after installation so the extension starts
`tracedecay lsp bridge --stdio` with `vscode-languageclient`.

The extension forwards bounded native diagnostics only for the single admitted
workspace folder. It sends URI, document version, range, source, message, and
safe diagnostic metadata—never document text or arbitrary diagnostic payloads.
The gateway merges that native upstream lane internally but, in Cursor-native
mode, publishes only TraceDecay findings back to avoid duplicating Cursor's own
diagnostics. Multi-root workspaces remain disabled until PR15.

The component is deployed at
`~/.cursor/extensions/tracedecay.cursor-native-0.0.0/`; its receipt and
installed manifest/bundle are checked by `tracedecay doctor --agent cursor`.
TraceDecay does not install or claim ownership of `rust-analyzer`,
`typescript-language-server`, Pyright, or another language analyzer.

For compiler output Cursor already captured, call `tracedecay_diagnose` first:
it maps the supplied `cargo`/`clippy` stderr to symbols and callers without
starting a toolchain. Use `tracedecay_diagnostics` only when fresh structured
diagnostics are needed; it runs the relevant type checker, so respect Cursor's
approval/run mode even though the tool does not edit the workspace.

`tracedecay lsp servers [--json]` is the separate PR12 CLI discovery command
for supported local language servers and install hints. It is informational:
it does not install or start a server. It is **not** an MCP tool, so do not add
it (or a wildcard) to `mcpAllowlist`.

For literal strings, regexes, and config keys inside indexed code, use
`tracedecay_grep`; reserve `tracedecay_search` for symbol names and
`tracedecay_context` for concept-level discovery.

For sessions resumed from compacted context, the `sessionStart` hook adds a
short recovery hint through Cursor's `additional_context` channel so the agent
knows to query TraceDecay LCM/session recall before assuming the compacted
summary is complete.

Slash workflows ship as Cursor-native commands
(`/tracedecay-map-architecture`, `/tracedecay-check-health`,
`/tracedecay-curate-memory`, `/tracedecay-review-diff`, ...). Their slugs keep
the `tracedecay-` prefix so typing `/tracedecay` lists every command.

## Auto-review and `permissions.json`

Since Cursor 3.6, Auto-review is the default run mode: every MCP call that is
not allowlisted goes through a classifier subagent before it runs, which adds
latency to every TraceDecay call. The plugin does **not** install
`permissions.json` for you (when `permissions.json` defines `mcpAllowlist`, it
*replaces* your in-app MCP allowlist entirely, so installing one silently would
clobber your settings). To let TraceDecay's read-only tools run without
per-call review, add the snippet below to `~/.cursor/permissions.json`
(per-user) or `<workspace>/.cursor/permissions.json` (per-repo):

```json
{
  "mcpAllowlist": [
    "tracedecay:tracedecay_active_project",
    "tracedecay:tracedecay_affected",
    "tracedecay:tracedecay_analytics",
    "tracedecay:tracedecay_api_migration_plan",
    "tracedecay:tracedecay_ast_grep_search",
    "tracedecay:tracedecay_automation_run_artifact_view",
    "tracedecay:tracedecay_body",
    "tracedecay:tracedecay_branch_diff",
    "tracedecay:tracedecay_branch_list",
    "tracedecay:tracedecay_branch_search",
    "tracedecay:tracedecay_by_qualified_name",
    "tracedecay:tracedecay_call_chain",
    "tracedecay:tracedecay_callees",
    "tracedecay:tracedecay_callers",
    "tracedecay:tracedecay_callers_for",
    "tracedecay:tracedecay_changelog",
    "tracedecay:tracedecay_circular",
    "tracedecay:tracedecay_code_callees",
    "tracedecay:tracedecay_code_callers",
    "tracedecay:tracedecay_code_declaration",
    "tracedecay:tracedecay_code_definition",
    "tracedecay:tracedecay_code_exact_occurrence",
    "tracedecay:tracedecay_code_facets",
    "tracedecay:tracedecay_code_implementations",
    "tracedecay:tracedecay_code_phrase_search",
    "tracedecay:tracedecay_code_references",
    "tracedecay:tracedecay_code_signature_search",
    "tracedecay:tracedecay_code_symbol_search",
    "tracedecay:tracedecay_code_timeline",
    "tracedecay:tracedecay_code_type_definition",
    "tracedecay:tracedecay_code_type_hierarchy",
    "tracedecay:tracedecay_commit_context",
    "tracedecay:tracedecay_complexity",
    "tracedecay:tracedecay_config",
    "tracedecay:tracedecay_configuration_audit",
    "tracedecay:tracedecay_configuration_explain",
    "tracedecay:tracedecay_configuration_get",
    "tracedecay:tracedecay_configuration_list",
    "tracedecay:tracedecay_configuration_observed_state",
    "tracedecay:tracedecay_configuration_protected_preview",
    "tracedecay:tracedecay_configuration_rollback_preview",
    "tracedecay:tracedecay_constructors",
    "tracedecay:tracedecay_context",
    "tracedecay:tracedecay_context_scout_budget",
    "tracedecay:tracedecay_context_scout_capability",
    "tracedecay:tracedecay_context_scout_explain",
    "tracedecay:tracedecay_context_scout_recent",
    "tracedecay:tracedecay_context_scout_status",
    "tracedecay:tracedecay_coupling",
    "tracedecay:tracedecay_dashboard",
    "tracedecay:tracedecay_dead_code",
    "tracedecay:tracedecay_dependency_depth",
    "tracedecay:tracedecay_derives",
    "tracedecay:tracedecay_diagnose",
    "tracedecay:tracedecay_diagnostics",
    "tracedecay:tracedecay_diagnostics_read",
    "tracedecay:tracedecay_diff_context",
    "tracedecay:tracedecay_distribution",
    "tracedecay:tracedecay_doc_coverage",
    "tracedecay:tracedecay_dsm",
    "tracedecay:tracedecay_feedback_advisory_cycle",
    "tracedecay:tracedecay_field_sites",
    "tracedecay:tracedecay_file_dependents",
    "tracedecay:tracedecay_file_metadata",
    "tracedecay:tracedecay_files",
    "tracedecay:tracedecay_find_exact_symbol",
    "tracedecay:tracedecay_gini",
    "tracedecay:tracedecay_git_blame",
    "tracedecay:tracedecay_git_diff",
    "tracedecay:tracedecay_git_history",
    "tracedecay:tracedecay_git_hunks",
    "tracedecay:tracedecay_git_preview",
    "tracedecay:tracedecay_git_status",
    "tracedecay:tracedecay_god_class",
    "tracedecay:tracedecay_grep",
    "tracedecay:tracedecay_health",
    "tracedecay:tracedecay_health_delta",
    "tracedecay:tracedecay_health_read",
    "tracedecay:tracedecay_hermes_skill_bridge",
    "tracedecay:tracedecay_hotspots",
    "tracedecay:tracedecay_impact",
    "tracedecay:tracedecay_implementations",
    "tracedecay:tracedecay_impls",
    "tracedecay:tracedecay_inheritance_depth",
    "tracedecay:tracedecay_largest",
    "tracedecay:tracedecay_lcm_describe",
    "tracedecay:tracedecay_lcm_expand",
    "tracedecay:tracedecay_lcm_expand_query",
    "tracedecay:tracedecay_lcm_grep",
    "tracedecay:tracedecay_lcm_load_session",
    "tracedecay:tracedecay_lcm_status",
    "tracedecay:tracedecay_message_search",
    "tracedecay:tracedecay_module_api",
    "tracedecay:tracedecay_node",
    "tracedecay:tracedecay_outline",
    "tracedecay:tracedecay_port_order",
    "tracedecay:tracedecay_port_status",
    "tracedecay:tracedecay_pr_context",
    "tracedecay:tracedecay_project_context",
    "tracedecay:tracedecay_project_list",
    "tracedecay:tracedecay_project_search",
    "tracedecay:tracedecay_qualified_name",
    "tracedecay:tracedecay_rank",
    "tracedecay:tracedecay_read",
    "tracedecay:tracedecay_recursion",
    "tracedecay:tracedecay_redundancy",
    "tracedecay:tracedecay_rename_preview",
    "tracedecay:tracedecay_retrieve",
    "tracedecay:tracedecay_runtime",
    "tracedecay:tracedecay_search",
    "tracedecay:tracedecay_session_lookup",
    "tracedecay:tracedecay_sessions_for",
    "tracedecay:tracedecay_signature",
    "tracedecay:tracedecay_signature_search",
    "tracedecay:tracedecay_similar",
    "tracedecay:tracedecay_simplify_scan",
    "tracedecay:tracedecay_skill_list",
    "tracedecay:tracedecay_skill_view",
    "tracedecay:tracedecay_source_body",
    "tracedecay:tracedecay_source_lines",
    "tracedecay:tracedecay_source_outline",
    "tracedecay:tracedecay_status",
    "tracedecay:tracedecay_storage_status",
    "tracedecay:tracedecay_test_map",
    "tracedecay:tracedecay_test_results",
    "tracedecay:tracedecay_test_risk",
    "tracedecay:tracedecay_todos",
    "tracedecay:tracedecay_type_hierarchy",
    "tracedecay:tracedecay_unsafe_patterns",
    "tracedecay:tracedecay_unused_imports",
    "tracedecay:tracedecay_workflows"
  ]
}
```

Notes:

- The list is exactly the tools that declare `readOnlyHint: true` - the edit
  primitives (`str_replace`, `replace_symbol`, ...), test runner, session
  baseline, memory writes, and LCM lifecycle tools are deliberately excluded
  so they keep going through review.
- Two borderline entries: `tracedecay_diagnostics` runs your toolchain
  (cargo/tsc/pyright) and `tracedecay_dashboard` starts a localhost server.
  Both are non-destructive, but remove those lines if you want a prompt first.
- `tracedecay_retrieve` only dereferences the required `handle` from a
  project-local truncated MCP response. Use it when omitted details are needed;
  it restores that exact cached response and does not re-run the source tool.
- Do **not** use `tracedecay:*` — it would auto-approve the editing tools too.
- Entries from per-user and per-repo files are concatenated; allowlists are a
  convenience, not a security boundary.

## Troubleshooting a dead MCP scope

Cursor spawns MCP servers with the user home directory as the working
directory, and it **never retries a failed MCP server**: if the `tracedecay
serve` process exits at startup (for example when a headless agent scope
passes a literal, unexpanded `${workspaceFolder}`), every later tool call in
that session reports "Timed out waiting for connection" until you toggle the
server or reload the window.

Two layers of defense ship with this plugin:

- `tracedecay serve` does not exit when project resolution fails at startup.
  It completes the MCP handshake and answers tool calls with an actionable
  error naming the failure and the fix; it rechecks the project on every tool
  call and recovers automatically once `tracedecay init` (or a corrected
  `--path`) makes resolution succeed.
- `tracedecay doctor --agent cursor` scans Cursor's recent MCP logs
  (`~/.config/Cursor/logs` on Linux, `~/Library/Application Support/Cursor/logs`
  on macOS, `%APPDATA%\Cursor\logs` on Windows) for tracedecay spawn failures —
  literal `${workspaceFolder}` errors, `Connection failed: MCP error -32000`,
  degraded-mode notices — and checks that the installed plugin bundle version
  matches the binary.

If a scope has already failed: fix the cause (usually `tracedecay init` in the
project, or upgrading a stale plugin with `tracedecay update-plugin`), then
toggle the tracedecay MCP server in Cursor Settings → MCP or reload the Cursor
window.

## Known limitations

- **Cloud agents:** plugin `sessionStart`, `sessionEnd`, `beforeSubmitPrompt`,
  `workspaceOpen`, and `stop` hooks never run in Cursor cloud agents, so the
  TraceDecay steering context and transcript ingest are desktop-only.
  Cloud agents do run repo-level `.cursor/hooks.json` hooks for the supported
  subset (`afterFileEdit`, `afterShellExecution`, tool hooks, subagent hooks).
- The plugin's session-recall tools only see transcripts ingested on this
  machine.

## Local development

For local development, install the generated Cursor projection after edits:

```bash
tracedecay install --agent cursor
```

The install path rewrites hook/MCP commands to the absolute binary path and
maps Cursor-specific overlays into their deployed locations. Reload Cursor
after reinstalling.
