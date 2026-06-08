---
name: project-status
description: Report tokensave index status, query config files, list outstanding TODO/FIXME markers, and triage MCP server runtime usage. Use for "tokensave status", "what version is dependency X", "what TODOs are left", "is the index fresh", or diagnosing tokensave CPU/RAM.
---

# Project status & config

Cheap, read-only surface for the index, project config, work markers, and server health.

## Workflow

1. **Index status → `tokensave_status`** (no args): node/edge/file counts, DB size, active branch + any branch-fallback warning, tokens saved. Add `tokensave_distribution` (`path?`, `summary?`) / `tokensave_files` (`path?`, `pattern?`) for a kind/file breakdown.
2. **Config lookups → `tokensave_config`** (`key` required, plus `path` for one file **or** `glob` for many): query TOML/JSON by dotted key (e.g. `key: "package.version"` on `Cargo.toml`, or `glob: "crates/*/Cargo.toml"`). Pure filesystem parse — works even before `tokensave init`.
3. **Outstanding work → `tokensave_todos`** (`kinds?`, `path?`, `limit?`): TODO/FIXME/XXX/HACK/WIP/NOTE/UNIMPLEMENTED markers with the enclosing symbol.
4. **Server triage → `tokensave_runtime`** (no args): PID, resident/virtual memory, CPU%, threads, DB/WAL/SHM sizes — use when tokensave seems to hog CPU or RAM.
5. **Memory subsystem health (optional) → `tokensave_memory_status`** (repairs derived vectors/banks; returns fact/entity counts + trust distribution).

## Guardrails

- `tokensave_status`, `tokensave_config`, `tokensave_todos`, `tokensave_runtime` are read-only. `tokensave_memory_status` repairs/normalizes memory state — run it only when memory looks stale or you need the counts.
- For deeper structural/quality questions hand off to `tokensave:architecture-overview` or `tokensave:code-health-report`; for memory recall, `tokensave:recalling-project-memory`.

## Output

- The status numbers, config values (with the file + line each was found at), marker list, or runtime snapshot the user asked for.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
