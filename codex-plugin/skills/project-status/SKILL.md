---
name: project-status
description: Use when checking tracedecay index freshness, project file/config status, TODO/FIXME markers, MCP runtime CPU/RAM, dashboard URL, or memory subsystem counts.
---

# Project status & config

Cheap, read-only surface for active project identity, resolved storage, the index, project config, work markers, and server health.

## Workflow

1. **Active project → `tracedecay_active_project`** (no args): resolved project root, scope prefix, branch identity, and the resolved active project store backing this session. Use this before describing where data lives.
2. **Storage status → `tracedecay_storage_status`** (no args): resolved active project store health, graph DB path, writability, branch fallback warnings, and counts. Use this instead of probing `.tracedecay` or running direct SQLite checks.
3. **Project registry → `tracedecay_project_list` / `tracedecay_project_search` / `tracedecay_project_context`**: list known projects, search by name/root, or load a registered project's resolved context when the user asks about another project or workspace.
4. **Index status → `tracedecay_status`** (no args): node/edge/file counts, DB size, active branch + any branch-fallback warning, tokens saved. Add `tracedecay_distribution` (`path?`, `summary?`) / `tracedecay_files` (`path?`, `pattern?`) for a kind/file breakdown.
5. **Config lookups → `tracedecay_config`** (`key` required, plus `path` for one file **or** `glob` for many): query TOML/JSON by dotted key (e.g. `key: "package.version"` on `Cargo.toml`, or `glob: "crates/*/Cargo.toml"`). Pure filesystem parse — works even before `tracedecay init`.
6. **Outstanding work → `tracedecay_todos`** (`kinds?`, `path?`, `limit?`): TODO/FIXME/XXX/HACK/WIP/NOTE/UNIMPLEMENTED markers with the enclosing symbol.
7. **Server triage → `tracedecay_runtime`** (no args): PID, resident/virtual memory, CPU%, threads, DB/WAL/SHM sizes — use when TraceDecay seems to hog CPU or RAM.
8. **User wants a visual → `tracedecay_dashboard`** (`action`: `start`|`stop`, optional `host`/`port`): starts the local dashboard server in the background and returns its URL (idempotent; `stop` shuts it down). Hand the URL to the user instead of describing charts.
9. **Memory subsystem health (optional) → `tracedecay_memory_status`** (repairs derived vectors/banks; returns fact/entity counts + trust distribution).

## Guardrails

- `tracedecay_active_project`, `tracedecay_storage_status`, `tracedecay_project_list`, `tracedecay_project_search`, `tracedecay_project_context`, `tracedecay_status`, `tracedecay_config`, `tracedecay_todos`, `tracedecay_runtime` are read-only. `tracedecay_dashboard` starts/stops a local server and `tracedecay_memory_status` repairs/normalizes memory state — use them only when the user wants the dashboard or memory counts.
- For deeper structural/quality questions hand off to `tracedecay:architecture-overview` or `tracedecay:code-health-report`; for memory recall, `tracedecay:recalling-project-memory`; for memory curation/update/delete, `tracedecay:curating-project-memory`; for past-session recall, `tracedecay:recalling-session-context`.

## Output

- The active project, resolved active project store, status numbers, config values (with the file + line each was found at), marker list, or runtime snapshot the user asked for.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
