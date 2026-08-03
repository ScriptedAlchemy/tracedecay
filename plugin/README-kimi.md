# TraceDecay Kimi Code Plugin

Stage this bundle for host-native installation by running:

```bash
tracedecay install --agent kimi
```

TraceDecay writes a complete plugin tree to
`~/.tracedecay/host-bundle-stage/kimi/tracedecay` and prints the exact deferred
remediation. Kimi Code owns its plugin registry, so TraceDecay does not edit
`$KIMI_CODE_HOME/plugins/installed.json` or the current managed plugin tree.
For install and update, open Kimi Code and run:

```text
/plugins install <staged-path>
```

Replace `<staged-path>` with the exact path printed by TraceDecay. For
uninstall, run this in Kimi Code:

```text
/plugins remove tracedecay
```

Then re-run TraceDecay repair or doctor to verify the host-native registration.
`KIMI_CODE_HOME` resolves to the environment variable when set and
`~/.kimi-code` otherwise. The staged manifest rewrites the MCP server command
to the resolved absolute `tracedecay` executable path so Kimi Code does not
depend on shell `PATH`.

Run `/reload` or start a new session after installing, updating, or removing the plugin:
Kimi Code picks up manifest, skill, and command changes only on reload. The
`/plugins` manager lists the installed TraceDecay plugin and its state.

The plugin registers the TraceDecay MCP server under the `tracedecay` key as:

```bash
tracedecay serve
```

The manifest also registers Kimi's native `PostToolUse` and `Stop` hooks.
Their stdin JSON is deliberately not forwarded or persisted: successful edit
tools trigger `tracedecay sync`, while `Stop` triggers
`tracedecay sessions ingest`. The installer renders both commands with the
same absolute TraceDecay binary path used by MCP.

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

For local development, stage the generated Kimi projection after edits:

```bash
tracedecay install --agent kimi
```

Complete the printed `/plugins install <staged-path>` action in Kimi Code. The
staged manifest rewrites the MCP command to the absolute binary path. Run
`/reload` (or start a new session) after replacing the plugin.
