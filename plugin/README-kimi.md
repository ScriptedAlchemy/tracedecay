# TraceDecay Kimi Code Plugin

This bundle is installed by:

```bash
tracedecay install --agent kimi
```

The installer writes a real plugin directory (not a symlink) to
`$KIMI_CODE_HOME/plugins/managed/tracedecay/`, where `KIMI_CODE_HOME` resolves
to the `$KIMI_CODE_HOME` environment variable when set and `~/.kimi-code`
otherwise. The MCP server command is rewritten to the resolved absolute
`tracedecay` executable path so Kimi Code does not depend on shell `PATH`.

Run `/reload` or start a new session after installing or replacing the plugin:
Kimi Code picks up manifest, skill, and command changes only on reload. The
`/plugins` manager lists the installed TraceDecay plugin and its state.

The plugin registers the TraceDecay MCP server under the `tracedecay` key as:

```bash
tracedecay serve
```

`serve` resolves the active project by walking up from the working directory
and then through the global project registry, so each indexed project keeps
its own `.tracedecay/` store. If tools report that no project is registered,
run `tracedecay init` in the project first.

Every MCP tool is also available from the shell as `tracedecay tool <name>`
(`tracedecay tool` lists tools; `tracedecay tool <name> --help` shows
parameters). The bundled `using-the-cli` skill uses that CLI fallback when MCP
transport errors or times out, instead of querying `.tracedecay` databases.

For literal strings, regexes, and config keys inside indexed code, use
`tracedecay_grep`; reserve `tracedecay_search` for symbol names and
`tracedecay_context` for concept-level discovery.

The 15 shared skills ship in the standard `SKILL.md` format under `skills/`.
The 13 workflow slash commands ship as Markdown with YAML frontmatter under
`commands/` and are namespaced by the plugin id: `/tracedecay:map-architecture`,
`/tracedecay:check-health`, `/tracedecay:review-diff`, and so on. Text typed
after the command replaces `$ARGUMENTS` in the command body.

## Local development

For checkout dogfooding, install the generated Kimi projection after edits:

```bash
tracedecay install --agent kimi
```

The install path rewrites the MCP command to the absolute binary path. Run
`/reload` (or start a new session) after reinstalling.
