# TraceDecay Cursor Plugin

This bundle is installed by:

```bash
tracedecay install --agent cursor
```

Reload Cursor after installing or replacing the plugin. `tracedecay install
--agent cursor` writes a real plugin directory rather than a symlink and rewrites
MCP/hook commands to the resolved absolute `tracedecay` executable path so
GUI-launched Cursor does not depend on shell `PATH`.

The plugin registers the `tracedecay` MCP server as:

```bash
tracedecay serve --path ${workspaceFolder}
```

Each Cursor workspace gets its own `.tracedecay/` index. Cursor's MCP runner
resolves `${workspaceFolder}` in normal editor windows.

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
    "graph:tracedecay_active_project",
    "graph:tracedecay_affected",
    "graph:tracedecay_analytics",
    "graph:tracedecay_automation_run_artifact_view",
    "graph:tracedecay_body",
    "graph:tracedecay_branch_diff",
    "graph:tracedecay_branch_list",
    "graph:tracedecay_branch_search",
    "graph:tracedecay_by_qualified_name",
    "graph:tracedecay_call_chain",
    "graph:tracedecay_callees",
    "graph:tracedecay_callers",
    "graph:tracedecay_callers_for",
    "graph:tracedecay_changelog",
    "graph:tracedecay_circular",
    "graph:tracedecay_commit_context",
    "graph:tracedecay_complexity",
    "graph:tracedecay_config",
    "graph:tracedecay_constructors",
    "graph:tracedecay_context",
    "graph:tracedecay_coupling",
    "graph:tracedecay_dashboard",
    "graph:tracedecay_dead_code",
    "graph:tracedecay_dependency_depth",
    "graph:tracedecay_derives",
    "graph:tracedecay_diagnose",
    "graph:tracedecay_diagnostics",
    "graph:tracedecay_diff_context",
    "graph:tracedecay_distribution",
    "graph:tracedecay_doc_coverage",
    "graph:tracedecay_dsm",
    "graph:tracedecay_field_sites",
    "graph:tracedecay_file_dependents",
    "graph:tracedecay_files",
    "graph:tracedecay_find_exact_symbol",
    "graph:tracedecay_gini",
    "graph:tracedecay_god_class",
    "graph:tracedecay_grep",
    "graph:tracedecay_health",
    "graph:tracedecay_hermes_skill_bridge",
    "graph:tracedecay_hotspots",
    "graph:tracedecay_impact",
    "graph:tracedecay_implementations",
    "graph:tracedecay_impls",
    "graph:tracedecay_inheritance_depth",
    "graph:tracedecay_largest",
    "graph:tracedecay_lcm_describe",
    "graph:tracedecay_lcm_expand",
    "graph:tracedecay_lcm_expand_query",
    "graph:tracedecay_lcm_grep",
    "graph:tracedecay_lcm_load_session",
    "graph:tracedecay_lcm_status",
    "graph:tracedecay_message_search",
    "graph:tracedecay_module_api",
    "graph:tracedecay_node",
    "graph:tracedecay_outline",
    "graph:tracedecay_port_order",
    "graph:tracedecay_port_status",
    "graph:tracedecay_pr_context",
    "graph:tracedecay_project_context",
    "graph:tracedecay_project_list",
    "graph:tracedecay_project_search",
    "graph:tracedecay_rank",
    "graph:tracedecay_read",
    "graph:tracedecay_recursion",
    "graph:tracedecay_redundancy",
    "graph:tracedecay_rename_preview",
    "graph:tracedecay_retrieve",
    "graph:tracedecay_runtime",
    "graph:tracedecay_search",
    "graph:tracedecay_sessions_for",
    "graph:tracedecay_signature",
    "graph:tracedecay_signature_search",
    "graph:tracedecay_similar",
    "graph:tracedecay_simplify_scan",
    "graph:tracedecay_skill_list",
    "graph:tracedecay_skill_view",
    "graph:tracedecay_status",
    "graph:tracedecay_storage_status",
    "graph:tracedecay_test_map",
    "graph:tracedecay_test_risk",
    "graph:tracedecay_todos",
    "graph:tracedecay_type_hierarchy",
    "graph:tracedecay_unsafe_patterns",
    "graph:tracedecay_unused_imports",
    "graph:tracedecay_workflows"
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

For checkout dogfooding, install the generated Cursor projection after edits:

```bash
tracedecay install --agent cursor
```

The install path rewrites hook/MCP commands to the absolute binary path and
maps Cursor-specific overlays into their deployed locations. Reload Cursor
after reinstalling.
