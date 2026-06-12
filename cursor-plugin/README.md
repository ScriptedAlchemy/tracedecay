# TokenSave Cursor Plugin

This bundle is installed by:

```bash
tokensave install --agent cursor
```

Reload Cursor after installing or replacing the plugin. `tokensave install
--agent cursor` writes a real plugin directory rather than a symlink and rewrites
MCP/hook commands to the resolved absolute `tokensave` executable path so
GUI-launched Cursor does not depend on shell `PATH`.

The plugin registers the tokensave MCP server as:

```bash
tokensave serve --path ${workspaceFolder}
```

This is intentionally workspace-scoped: each Cursor workspace uses its own
`.tokensave/` index instead of the legacy global Cursor MCP registration. The
`${workspaceFolder}` variable is resolved by Cursor's MCP runner; if your Cursor
build does not expand it, reinstall with the latest Cursor and run
`tokensave doctor --agent cursor` to inspect the generated plugin config.

Hook commands derive the active project from Cursor's event payload /
`CURSOR_PROJECT_DIR`, not from the plugin directory.

Slash workflows ship as skills with `disable-model-invocation: true`
(`/tokensave-map-architecture`, `/tokensave-check-health`,
`/tokensave-review-diff`, …) — Cursor's Commands surface was absorbed into
Skills, so this bundle no longer ships a `commands/` directory. Their slugs
keep the `tokensave-` prefix so typing `/tokensave` lists every command, and
the suffix is a verb phrase so the human-facing title (Cursor displays the
humanized slug) reads as the action it performs.

## Auto-review and `permissions.json`

Since Cursor 3.6, Auto-review is the default run mode: every MCP call that is
not allowlisted goes through a classifier subagent before it runs, which adds
latency to every tokensave call. The plugin does **not** install
`permissions.json` for you (when `permissions.json` defines `mcpAllowlist`, it
*replaces* your in-app MCP allowlist entirely, so installing one silently would
clobber your settings). To let tokensave's read-only tools run without
per-call review, add the snippet below to `~/.cursor/permissions.json`
(per-user) or `<workspace>/.cursor/permissions.json` (per-repo):

```json
{
  "mcpAllowlist": [
    "tokensave:tokensave_affected",
    "tokensave:tokensave_body",
    "tokensave:tokensave_branch_diff",
    "tokensave:tokensave_branch_list",
    "tokensave:tokensave_branch_search",
    "tokensave:tokensave_by_qualified_name",
    "tokensave:tokensave_call_chain",
    "tokensave:tokensave_callees",
    "tokensave:tokensave_callers",
    "tokensave:tokensave_callers_for",
    "tokensave:tokensave_changelog",
    "tokensave:tokensave_circular",
    "tokensave:tokensave_commit_context",
    "tokensave:tokensave_complexity",
    "tokensave:tokensave_config",
    "tokensave:tokensave_constructors",
    "tokensave:tokensave_context",
    "tokensave:tokensave_coupling",
    "tokensave:tokensave_dashboard",
    "tokensave:tokensave_dead_code",
    "tokensave:tokensave_dependency_depth",
    "tokensave:tokensave_derives",
    "tokensave:tokensave_diagnose",
    "tokensave:tokensave_diagnostics",
    "tokensave:tokensave_diff_context",
    "tokensave:tokensave_distribution",
    "tokensave:tokensave_doc_coverage",
    "tokensave:tokensave_dsm",
    "tokensave:tokensave_field_sites",
    "tokensave:tokensave_file_dependents",
    "tokensave:tokensave_files",
    "tokensave:tokensave_find_exact_symbol",
    "tokensave:tokensave_gini",
    "tokensave:tokensave_god_class",
    "tokensave:tokensave_health",
    "tokensave:tokensave_hotspots",
    "tokensave:tokensave_impact",
    "tokensave:tokensave_implementations",
    "tokensave:tokensave_impls",
    "tokensave:tokensave_inheritance_depth",
    "tokensave:tokensave_largest",
    "tokensave:tokensave_lcm_describe",
    "tokensave:tokensave_lcm_expand",
    "tokensave:tokensave_lcm_expand_query",
    "tokensave:tokensave_lcm_grep",
    "tokensave:tokensave_lcm_load_session",
    "tokensave:tokensave_lcm_status",
    "tokensave:tokensave_message_search",
    "tokensave:tokensave_module_api",
    "tokensave:tokensave_node",
    "tokensave:tokensave_outline",
    "tokensave:tokensave_port_order",
    "tokensave:tokensave_port_status",
    "tokensave:tokensave_pr_context",
    "tokensave:tokensave_rank",
    "tokensave:tokensave_read",
    "tokensave:tokensave_recursion",
    "tokensave:tokensave_redundancy",
    "tokensave:tokensave_rename_preview",
    "tokensave:tokensave_retrieve",
    "tokensave:tokensave_runtime",
    "tokensave:tokensave_search",
    "tokensave:tokensave_signature",
    "tokensave:tokensave_signature_search",
    "tokensave:tokensave_similar",
    "tokensave:tokensave_simplify_scan",
    "tokensave:tokensave_status",
    "tokensave:tokensave_test_map",
    "tokensave:tokensave_test_risk",
    "tokensave:tokensave_todos",
    "tokensave:tokensave_type_hierarchy",
    "tokensave:tokensave_unsafe_patterns",
    "tokensave:tokensave_unused_imports"
  ]
}
```

Notes:

- The list is exactly the tools that declare `readOnlyHint: true` — the edit
  primitives (`str_replace`, `replace_symbol`, …), test runner, session
  baseline, memory writes, and LCM lifecycle tools are deliberately excluded
  so they keep going through review.
- Two borderline entries: `tokensave_diagnostics` runs your toolchain
  (cargo/tsc/pyright) and `tokensave_dashboard` starts a localhost server.
  Both are non-destructive, but remove those lines if you want a prompt first.
- `tokensave_retrieve` only dereferences the required `handle` from a
  project-local truncated MCP response. Use it when omitted details are needed;
  it restores that exact cached response and does not re-run the source tool.
- Do **not** use `tokensave:*` — it would auto-approve the editing tools too.
- Entries from per-user and per-repo files are concatenated; allowlists are a
  convenience, not a security boundary.

## Known limitations

- **Cloud agents:** plugin `sessionStart`, `sessionEnd`, `beforeSubmitPrompt`,
  `workspaceOpen`, and `stop` hooks never run in Cursor cloud agents, so the
  tokensave steering context and transcript ingest are desktop-only today.
  Cloud agents do run repo-level `.cursor/hooks.json` hooks for the supported
  subset (`afterFileEdit`, `afterShellExecution`, tool hooks, subagent hooks).
- The plugin's session-recall tools only see transcripts ingested on this
  machine.

## Local development

For checkout dogfooding, Cursor's docs bless symlinking the bundle into the
local plugin directory so edits are picked up without reinstalling:

```bash
mkdir -p ~/.cursor/plugins/local
rm -rf ~/.cursor/plugins/local/tokensave
ln -s /path/to/tokensave/cursor-plugin ~/.cursor/plugins/local/tokensave
```

Caveat: a symlinked bundle keeps the literal `tokensave …` hook/MCP commands,
so GUI-launched Cursor must be able to resolve `tokensave` on `PATH` (the real
install rewrites them to an absolute binary path). Copying the directory
(`cp -R` instead of `ln -s`) also works. Reload Cursor after either change.
