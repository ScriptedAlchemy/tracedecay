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

This is intentionally workspace-scoped: each Cursor workspace uses its own
`.tracedecay/` index instead of the legacy global Cursor MCP registration. The
`${workspaceFolder}` variable is resolved by Cursor's MCP runner; if your Cursor
build does not expand it, reinstall with the latest Cursor and run
`tracedecay doctor --agent cursor` to inspect the generated plugin config.

Hook commands derive the active project from Cursor's event payload /
`CURSOR_PROJECT_DIR`, not from the plugin directory.

Slash workflows ship as skills with `disable-model-invocation: true`
(`/tracedecay-map-architecture`, `/tracedecay-check-health`,
`/tracedecay-review-diff`, ...) - Cursor's Commands surface was absorbed into
Skills, so this bundle no longer ships a `commands/` directory. Their slugs
keep the `tracedecay-` prefix so typing `/tracedecay` lists every command, and
the suffix is a verb phrase so the human-facing title (Cursor displays the
humanized slug) reads as the action it performs.

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
    "tracedecay:tracedecay_affected",
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
    "tracedecay:tracedecay_commit_context",
    "tracedecay:tracedecay_complexity",
    "tracedecay:tracedecay_config",
    "tracedecay:tracedecay_constructors",
    "tracedecay:tracedecay_context",
    "tracedecay:tracedecay_coupling",
    "tracedecay:tracedecay_dashboard",
    "tracedecay:tracedecay_dead_code",
    "tracedecay:tracedecay_dependency_depth",
    "tracedecay:tracedecay_derives",
    "tracedecay:tracedecay_diagnose",
    "tracedecay:tracedecay_diagnostics",
    "tracedecay:tracedecay_diff_context",
    "tracedecay:tracedecay_distribution",
    "tracedecay:tracedecay_doc_coverage",
    "tracedecay:tracedecay_dsm",
    "tracedecay:tracedecay_field_sites",
    "tracedecay:tracedecay_file_dependents",
    "tracedecay:tracedecay_files",
    "tracedecay:tracedecay_find_exact_symbol",
    "tracedecay:tracedecay_gini",
    "tracedecay:tracedecay_god_class",
    "tracedecay:tracedecay_health",
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
    "tracedecay:tracedecay_rank",
    "tracedecay:tracedecay_read",
    "tracedecay:tracedecay_recursion",
    "tracedecay:tracedecay_redundancy",
    "tracedecay:tracedecay_rename_preview",
    "tracedecay:tracedecay_runtime",
    "tracedecay:tracedecay_search",
    "tracedecay:tracedecay_signature",
    "tracedecay:tracedecay_signature_search",
    "tracedecay:tracedecay_similar",
    "tracedecay:tracedecay_simplify_scan",
    "tracedecay:tracedecay_status",
    "tracedecay:tracedecay_test_map",
    "tracedecay:tracedecay_test_risk",
    "tracedecay:tracedecay_todos",
    "tracedecay:tracedecay_type_hierarchy",
    "tracedecay:tracedecay_unsafe_patterns",
    "tracedecay:tracedecay_unused_imports"
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
- Do **not** use `tracedecay:*` - it would auto-approve the editing tools too.
- Entries from per-user and per-repo files are concatenated; allowlists are a
  convenience, not a security boundary.

## Known limitations

- **Cloud agents:** plugin `sessionStart`, `sessionEnd`, `beforeSubmitPrompt`,
  `workspaceOpen`, and `stop` hooks never run in Cursor cloud agents, so the
  TraceDecay steering context and transcript ingest are desktop-only today.
  Cloud agents do run repo-level `.cursor/hooks.json` hooks for the supported
  subset (`afterFileEdit`, `afterShellExecution`, tool hooks, subagent hooks).
- The plugin's session-recall tools only see transcripts ingested on this
  machine.

## Local development

For checkout dogfooding, Cursor's docs bless symlinking the bundle into the
local plugin directory so edits are picked up without reinstalling:

```bash
mkdir -p ~/.cursor/plugins/local
rm -rf ~/.cursor/plugins/local/tracedecay
ln -s /path/to/tracedecay/cursor-plugin ~/.cursor/plugins/local/tracedecay
```

Caveat: a symlinked bundle keeps the literal `tracedecay ...` hook/MCP commands,
so GUI-launched Cursor must be able to resolve `tracedecay` on `PATH` (the real
install rewrites them to an absolute binary path). Copying the directory
(`cp -R` instead of `ln -s`) also works. Reload Cursor after either change.
