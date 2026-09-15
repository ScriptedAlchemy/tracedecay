# TraceDecay profile pack — CONCEPT / PROFILE SNAPSHOT

This directory is a **read-only slim snapshot** for the throwaway HUD at `/workspace/td-brain-demo`.
It is **not** a production evidence set. Do not treat row counts, titles, or graph-head numbers as sealed index state.

Captured from Zack's Mac profile `~/.tracedecay` at **2026-08-30 1:08 AM PT**.

## What this is

- JSON spines extracted with `sqlite3 -json` (no message `text` bodies, no FTS, no full `observation_json` payloads).
- Small sidecar files: `global.db` (~4.1 MB), `hook_analytics.jsonl`, per-project `hook_analytics.jsonl` / `store_manifest.json` / `branch-meta.json`.
- For newly enrolled `core` (`proj_2c558f8ce42d5cdf`): local/remote branch name dumps only. Git repo and grafeo were **not** copied.

## What was NOT copied

- `*.grafeo` (mqvpn ~4 GB, zerofs-ios ~457 MB, ZeroFS ~406 MB)
- full `sessions.db` files (96–672 MB)
- `user-sessions.db` (~1.4 GB)
- message `text` bodies / FTS / `observation_json` full payloads

## Live DB caveats (do not treat as production)

- `session_threads` is **empty** in every live `sessions.db` (0 rows).
- Code index is **NOT sealed**.
  - PRIMARY `proj_ae394425f7837d4f` `tracedecay.db`: `graph_verified_heads_v1=1`, `retrieval_anchors=0`, no `facts` table.
  - Other projects: `retrieval_anchors=0`, no `facts` table; `graph_verified_heads_v1` is 2 (ZeroFS, zerofs-ios-companion, movie-library), 13 (mqvpn).
- `core` was just enrolled; sessions/messages/observations/agents are empty arrays.

## Row counts (sqlite live DBs at extract time)

| id | store_manifest project_root | sessions | messages (spine) | observations (slim) | agents | session_threads |
|---|---|---:|---:|---:|---:|---:|
| proj_ae394425f7837d4f PRIMARY | /Volumes/bigssd/projects/tracedecay | 4 | 229 | 658 | 0 | 0 |
| proj_e19f6f383c982ea8 | /Volumes/bigssd/projects/ZeroFS | 26 | 1662 | 2712 | 30 | 0 |
| proj_8007f36f3654e9be densest agents | /Volumes/bigssd/projects/zerofs-ios-companion | 40 | 5966 | 6980 | 49 | 0 |
| proj_a812b7edf6fab331 empty sessions | /Users/zackjackson/plugins/movie-library | 0 | 0 | 0 | 0 | 0 |
| proj_2f28a204e57c569f | /Volumes/bigssd/projects/mqvpn | 1 | 14 | 16 | 0 | 0 |
| proj_2c558f8ce42d5cdf newly enrolled | /Volumes/bigssd/projects/core | 0 | 0 | 0 | 0 | 0 |

## Copied sizes

See `manifest.json` for per-file byte sizes and row counts. Pack total is about **17 MB**.

### Top-level

- `global.db` — 4,071,424 bytes (CopyToBox **succeeded**; WAL/SHM not copied)
- `hook_analytics.jsonl` — 7,435,611 bytes / 21,992 lines (CopyToBox **succeeded**)
- `manifest.json` — this index
- `README.md` — this file

### Per project (bytes)

- `proj_ae394425f7837d4f/` sessions 1122, messages-spine 55136, observations-slim 152293, agents 3 (`[]`), hook_analytics.jsonl 778551, store_manifest 396, branch-meta 209
- `proj_e19f6f383c982ea8/` sessions 7557, messages-spine 388752, observations-slim 607683, agents 5815, hook_analytics.jsonl 951456, store_manifest 392, branch-meta 241
- `proj_8007f36f3654e9be/` sessions 12011, messages-spine 1366161, observations-slim 1522710, agents 9657, hook_analytics.jsonl 184632, store_manifest 406, branch-meta 205
- `proj_a812b7edf6fab331/` empty JSON arrays (3 bytes each); no hook_analytics.jsonl on disk; store_manifest 401, branch-meta 209
- `proj_2f28a204e57c569f/` sessions 278, messages-spine 3408, observations-slim 3785, agents 3 (`[]`), hook_analytics.jsonl 28125, store_manifest 391, branch-meta 205
- `proj_2c558f8ce42d5cdf/` empty JSON arrays; no hook_analytics.jsonl; store_manifest 390, branch-meta 205, branches-local.txt 23757, branches-remote.txt 10688 (plus sibling branch JSON/README written by the enroll pass)

## Loader note

HUD fixtures in `src/data/fixtures.ts` are a CONCEPT sketch. Prefer this pack when you need live spines. Empty `[]` arrays are real empty tables, not missing files.
