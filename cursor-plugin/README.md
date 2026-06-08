# TokenSave Cursor Plugin

This bundle is installed by:

```bash
tokensave install --agent cursor
```

For checkout dogfooding or manual install, copy the bundle into Cursor's local
plugin directory:

```bash
mkdir -p ~/.cursor/plugins/local
rm -rf ~/.cursor/plugins/local/tokensave
cp -R /path/to/tokensave/cursor-plugin ~/.cursor/plugins/local/tokensave
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
The plugin does not install `permissions.json`; approvals are left to Cursor
approval/run-mode behavior.
