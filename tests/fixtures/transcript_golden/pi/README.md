# Pi session fixture

`2026-09-25T16-00-00-000Z_5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90.jsonl` is a
synthetic session in the Pi coding-agent 0.87.1 session file format (version 3):
a `session` header, then tree entries linked by `id`/`parentId`. It follows
`packages/coding-agent/docs/session-format.md` and `message-types.md` at tag
`v0.87.1` of https://github.com/earendil-works/pi. No operator transcript was
copied; prompts, tool descriptions, and tool bodies are typed placeholders.

Tests copy the file to `<agent dir>/sessions/--<encoded cwd>--/` with
`<PROJECT_ROOT>` replaced by the test project, then run production discovery
and ingestion. The file name carries the session id, which must match the
header id.
