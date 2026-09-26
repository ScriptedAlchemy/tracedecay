# Droid lifecycle payloads

Factory Droid hooks are shell commands registered in `~/.factory/hooks.json`
(`hooks.json` keyed by event name, each event a list of matcher groups, each
group a list of `{ "type": "command", "command": "...", "timeout": n }`
entries). Droid runs every hook with one JSON object on stdin and closes
stdin. These fixtures are the captured payload shape for the events TraceDecay
ships:

- `SessionStart` — `session_id`, `transcript_path`, `cwd`, `permission_mode`,
  `hook_event_name`, `source`, and Droid's own `CLAUDE_ENV_FILE`.
- `Stop` — the same base fields plus `message_id`, `stop_hook_active`,
  `tool_execution_count`, and `elapsed_time`.

The TraceDecay integration deploys both events calling
`tracedecay hook-droid-event`, which parses this payload.

Reference: https://docs.factory.ai/harness/hooks
