# Kimi Code native hook captures

These payloads were captured from Kimi Code CLI 0.26.0 on 2026-07-21 using
temporary hooks declared in the enabled TraceDecay plugin manifest:

- `PostToolUse` matched only the built-in `Edit` tool.
- `Stop` matched the turn boundary.

Both hooks used a mode-`0600` append-only `tee` target under `/tmp`. The live
probe changed one temporary file from `before` to `after`. The plugin manifest
was restored byte-for-byte after capture, and `config.toml` was not modified.

Sanitization preserves the authentic JSON shape while replacing the session
ID, project root, saved path, tool call ID, edited text, and tool output with
typed placeholders.

Official references:

- https://moonshotai.github.io/kimi-code/en/customization/hooks.html
- https://moonshotai.github.io/kimi-code/en/customization/plugins.html#hooks-in-plugins
