# Cursor native hook captures

`after-file-edit.json` is a sanitized capture of the exact stdin delivered to
an `afterFileEdit` command hook by Cursor Agent CLI
`2026.07.09-a3815c0` on 2026-07-21.

The capture came from a headless local agent running in a restricted temporary
workspace. The agent used its native file-editing tool to update `probe.txt`.
A project-native `.cursor/hooks.json` command teed stdin to a mode-0600
temporary file before passing the same bytes to the installed TraceDecay
`hook-cursor-after-file-edit` handler.

Sanitization preserved the native object keys, value types, array lengths, and
field order. IDs, model, version, paths, email, and edited text were replaced
with deterministic angle-bracket placeholders. The raw capture and temporary
hook configuration were deleted after sanitization.

The same Cursor CLI process completed normally, but did not invoke its
configured `stop` command hook, so this directory intentionally contains no
claimed native `stop` fixture.
