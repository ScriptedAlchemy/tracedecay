---
name: project-status
description: Use when checking tracedecay index freshness, project file/config status, TODO/FIXME markers, MCP runtime CPU/RAM, dashboard URL, or memory subsystem counts.
---

# Project status & config

Cheap, read-only surface for the index, project config, work markers, and server health.

## Workflow

1. **Index status → `tracedecay_status`** (no args): node/edge/file counts, DB size, active branch + any branch-fallback warning, tokens saved. Add `tracedecay_distribution` (`path?`, `summary?`) / `tracedecay_files` (`path?`, `pattern?`) for a kind/file breakdown.
2. **Config lookups → `tracedecay_config`** (`key` required, plus `path` for one file **or** `glob` for many): query TOML/JSON by dotted key (e.g. `key: "package.version"` on `Cargo.toml`, or `glob: "crates/*/Cargo.toml"`). Pure filesystem parse — works even before `tracedecay init`.
3. **Outstanding work → `tracedecay_todos`** (`kinds?`, `path?`, `limit?`): TODO/FIXME/XXX/HACK/WIP/NOTE/UNIMPLEMENTED markers with the enclosing symbol.
4. **Server triage → `tracedecay_runtime`** (no args): PID, resident/virtual memory, CPU%, threads, DB/WAL/SHM sizes — use when TraceDecay seems to hog CPU or RAM.
5. **User wants a visual → `tracedecay_dashboard`** (`action`: `start`|`stop`, optional `host`/`port`): starts the local dashboard server in the background and returns its URL (idempotent; `stop` shuts it down). Hand the URL to the user instead of describing charts.
6. **Memory subsystem health (optional) → `tracedecay_memory_status`** (repairs derived vectors/banks; returns fact/entity counts + trust distribution).

## Guardrails

- `tracedecay_status`, `tracedecay_config`, `tracedecay_todos`, `tracedecay_runtime` are read-only. `tracedecay_dashboard` starts/stops a local server and `tracedecay_memory_status` repairs/normalizes memory state — use them only when the user wants the dashboard or memory counts.
- For deeper structural/quality questions hand off to `tracedecay:architecture-overview` or `tracedecay:code-health-report`; for memory recall, `tracedecay:recalling-project-memory`; for memory curation/update/delete, `tracedecay:curating-project-memory`; for past-session recall, `tracedecay:recalling-session-context`.

## Output

- The status numbers, config values (with the file + line each was found at), marker list, or runtime snapshot the user asked for.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
