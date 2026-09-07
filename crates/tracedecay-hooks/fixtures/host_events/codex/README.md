# Codex native hook captures

These fixtures come from native command-hook stdin emitted by `codex-cli
0.144.5` on 2026-07-21. Capture used a mode-`0700` recorder under `/tmp` that
teed stdin to the existing TraceDecay hook command. The recorder ran only for
isolated, ephemeral `codex exec` sessions in temporary Git repositories with
hook-trust bypass scoped to each invocation.

The installed hook file and Codex configuration were backed up before each
run and restored by cleanup traps. Raw payloads, temporary repositories,
recorders, command output, and backups were deleted after sanitization.

Deterministic replacements:

- session and turn identifiers: `<SESSION_ID>`, `<TURN_ID>`
- project and transcript paths: `<PROJECT_ROOT>`, `<TRANSCRIPT_PATH>`
- model and permission mode: `<MODEL>`, `<PERMISSION_MODE>`
- final assistant text: `<REDACTED_RESPONSE>`

`stop.json` is an authentic sanitized `Stop` payload. Its null
`transcript_path` is preserved from the ephemeral native event.

No `PostToolUse` stdin reached the restricted recorder for the requested
`apply_patch` operation. No post-tool fixture is checked in because deriving
one from rollout JSON, analytics, documentation, or an expected schema would
not be an authentic native-hook capture.

## Interactive edit retry

The missing edit event was retried through the interactive Codex TUI in a
deterministic `tmux` PTY. Before capture:

- the official Codex hooks documentation was checked for `PostToolUse`
  `apply_patch` support and the `Edit`/`Write` matcher aliases;
- `codex features list` reported `hooks` as stable and effectively enabled;
- the installed matcher was temporarily restricted to
  `^(apply_patch|Edit|Write)$`;
- the recorder sanitized native stdin before forwarding the same payload to
  the existing TraceDecay handler; and
- Codex displayed that hook-trust bypass was active for the invocation.

The TUI reached native `apply_patch`, but Codex reported that the patch failed
and did not create the target file. No `PostToolUse` stdin reached the
recorder. A final attempt with the temporary repository explicitly trusted
and writable likewise produced no edit event before bounded cleanup. The
matcher, handler, feature/config state, and project trust were restored
exactly, and all raw and temporary capture data was removed.
