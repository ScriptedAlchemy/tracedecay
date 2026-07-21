# OpenCode native plugin capture

`baseline.json` is the sanitized event bundle captured from OpenCode 1.18.4
with `@opencode-ai/plugin` 1.15.13 on 2026-07-21. The capture used a temporary
global local plugin in an isolated project and recorded the native
`file.edited`, `tool.execute.after`, `session.status`, and `session.idle`
payloads produced by one `opencode run` invocation.

Sanitization replaces project, session, call, event, patch, and result content
with deterministic placeholders while retaining the native object keys, value
types, event channels, and array shape. The checked-in bundle contains no raw
source text, credentials, user identity, or host paths.
