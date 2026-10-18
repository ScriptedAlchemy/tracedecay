# OpenCode native plugin capture

`baseline.json` is the sanitized event bundle captured from OpenCode 2.0.19
on 2026-09-29 with a temporary local V2 plugin (`@opencode/plugin` 2.0.19)
under an isolated `$XDG_CONFIG_HOME`, during a real
`opencode run --standalone --auto` edit of a sandbox file.

The plugin recorded every `execute.after` tool hook and every event on
`ctx.event.subscribe()`. The public V2 stream emitted no `file.edited`,
`filesystem.changed`, `session.idle`, `session.status`, or `lsp.updated` event
for that run; edits surface only through the tool hook, and the turn boundary
is the durable `session.execution.succeeded` event, which carries no
`location`. The bundle keeps one edit tool callback, one non-mutating tool
callback, and that boundary event.

Sanitization replaces project, session, message, call, and event identities,
timestamps and sequence numbers, and patch, string, and result content with
deterministic placeholders while retaining the native object keys, value
types, hook channels, and array shape. The SHA-256 of the raw capture log is
recorded in the bundle. The checked-in bundle contains no raw source text,
credentials, user identity, or host paths.
