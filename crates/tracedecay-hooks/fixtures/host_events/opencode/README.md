# OpenCode native plugin capture

`baseline.json` is the sanitized event bundle captured from OpenCode 1.18.4.
The edit/tool/session events were captured with `@opencode-ai/plugin` 1.15.13
on 2026-07-21. The `lsp.updated` event was captured with
`@opencode-ai/plugin` 1.18.4 on 2026-07-26 after an isolated custom LSP emitted
a standard diagnostic during a real `opencode run` edit. Both captures used a
temporary local plugin.

Sanitization replaces project, session, call, event, patch, and result content
with deterministic placeholders while retaining the native object keys, value
types, event channels, and array shape. The raw `lsp.updated` event digest is
recorded in the bundle. The checked-in bundle contains no raw source text,
credentials, user identity, or host paths.
