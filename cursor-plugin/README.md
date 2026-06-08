# TokenSave Cursor Plugin

This bundle is installed by:

```bash
tokensave install --agent cursor
```

For checkout dogfooding or manual install:

```bash
mkdir -p ~/.cursor/plugins/local
ln -s /path/to/tokensave/cursor-plugin ~/.cursor/plugins/local/tokensave
```

Reload Cursor after installing or replacing the plugin.

The plugin registers the tokensave MCP server as:

```bash
tokensave serve --path ${workspaceFolder}
```

Hook commands resolve `tokensave` from `PATH` and derive the active project from
Cursor's event payload / `CURSOR_PROJECT_DIR`, not from the plugin directory.
The plugin does not install `permissions.json`; approvals are left to Cursor
approval/run-mode behavior.
