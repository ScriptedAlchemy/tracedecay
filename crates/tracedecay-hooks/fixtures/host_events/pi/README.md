# Pi lifecycle payloads

Pi has no native hook protocol. Its extension API delivers `session_start` and
`agent_end` events in-process, and the TraceDecay extension
(`plugin/pi/index.ts`) forwards each one to `tracedecay hook-pi-event` as one
JSON object on stdin, then closes stdin. These fixtures are that payload shape:
`id` is a fresh UUID per event, `session_id` is Pi's session id, and `cwd` is
the session working directory. `reason` is Pi's `session_start` reason.

Reference: https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md
