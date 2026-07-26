# tracedecay Dashboard

The TraceDecay dashboard is a local single-page application with twelve
workspaces: Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery,
Automations, Observatory, Costs, and Settings. The real `dashboard/app-dist`
bundle is served at `/`; the retired plugin-shell placeholder is isolated at
`/legacy` for compatibility and is not the product dashboard. The dashboard
runs locally; public model-price refreshes are the only optional network access
used by the Costs workspace.

Delivery, Explorer, Loom, Settings, Doctor/Observatory, storage telemetry, and
bundled app serving are implemented in the Rust API. They remain **unverified
by the aggregate `dashboard_api_test` suite**, which has not completed
successfully on this branch; implementation and verification status must not
be conflated.

---

## Table of Contents

- [Quick Start](#quick-start)
- [Standalone Usage](#standalone-usage)
- [Hermes Integration](#hermes-integration)
- [Dashboard Tabs](#dashboard-tabs)
  - [Holographic Memory](#holographic-memory)
  - [LCM](#lcm)
  - [Code Graph](#code-graph)
  - [Savings & Cost](#savings--cost)
- [API Reference](#api-reference)
  - [Capability Discovery](#capability-discovery)
  - [Holographic Memory API](#holographic-memory-api)
  - [LCM API](#lcm-api)
  - [Savings & Cost API](#savings--cost-api)
  - [Settings API](#settings-api)
- [Capability Flags](#capability-flags)
- [Frontend Development](#frontend-development)
- [Troubleshooting](#troubleshooting)

---

## Quick Start

```bash
# Start the dashboard on the default port (7341)
tracedecay dashboard

# Output:
# tracedecay dashboard listening on http://127.0.0.1:7341/
# Serving project /home/user/my-project
# Press Ctrl+C to stop.

# Then open http://127.0.0.1:7341/ in your browser
```

---

## Standalone Usage

### Command-Line Flags

```bash
tracedecay dashboard [OPTIONS]

Options:
  -p, --path <PATH>  Project path (default: current directory, with discovery)
      --host <HOST>  Address to bind [default: 127.0.0.1]
      --port <PORT>  Port to listen on (0 = pick a free port) [default: 7341]
      --open         Open the dashboard URL in the default browser after the server starts
  -h, --help         Print help
```

### MCP Tool

MCP-connected agents can manage the dashboard without a terminal via the
`tracedecay_dashboard` tool. It starts the server for the current project as a
background task inside the MCP server and returns the listening URL.
Idempotent: if a dashboard is already running, the existing URL is returned.
Pass `action: "stop"` to shut it down; optional `host`/`port` arguments match
the CLI defaults.

### Port 0 (Auto-Select)

When `--port 0` is specified, the OS assigns a free port. The server prints a parseable URL on stdout as the first line:

```bash
tracedecay dashboard --port 0
# tracedecay dashboard listening on http://127.0.0.1:45678/
```

This format is stable and used by wrapper tools (like the Hermes plugin) to discover the server URL.

### Environment Variables

| Variable | Description |
|----------|-------------|
| `TRACEDECAY_GLOBAL_DB` | Pin the LCM session store to an explicit database path. When set, it wins over resolved project-store selection (`storage_scope` becomes `"global"`); when unset, the dashboard serves the active project's resolved user-level profile session store. |
| `TRACEDECAY_BIN` | Path to the tracedecay binary (used by Hermes wrapper for spawn mode) |
| `TRACEDECAY_DASHBOARD_PROJECT` | Project root path for Hermes dashboard spawn mode (defaults to Hermes' cwd) |
| `TRACEDECAY_DASHBOARD_URL` | Full URL to an already-running dashboard (Hermes external URL mode) |
| `TRACEDECAY_OFFLINE` | Set to `1` to skip network requests for pricing data (Savings & Cost tab uses bundled fallback) |
| `TRACEDECAY_MODEL_PRICES_PATH` | Override the on-disk model-price cache location (default `~/.tracedecay/model-prices.json`; mainly for tests) |
| `DISABLE_TRACEDECAY` | Set to `true` to disable the MCP server entirely (exits cleanly without initializing) |

Use `TRACEDECAY_*` variables (and `DISABLE_TRACEDECAY`) for dashboard runtime
configuration. Pre-rename `TRACEDECAY_*` spellings are not runtime fallbacks.
Hermes may separately configure a host home for its own config, plugins, and
transcripts; that host setting is not a TraceDecay environment variable and
never selects a TraceDecay store or project.

---

## Hermes Integration

The dashboard is the canonical implementation; the Hermes plugin is a thin wrapper that reuses it.

### Installation

`tracedecay install --agent hermes` deploys the wrapper once as a Hermes
dashboard plugin alongside the agent plugin, into
`~/.hermes/plugins/tracedecay/dashboard/` (`manifest.json`,
`plugin_api.py`, and the UI bundles — all embedded in the tracedecay binary,
no source checkout needed). Hermes' dashboard plugin discovery scans
`plugins/*/dashboard/manifest.json` in both stock and forked Hermes, so a
"TraceDecay" tab (Memory / LCM / Code Graph / Savings) appears in
`hermes dashboard` after install. Named profiles and project-local Hermes
homes are not separate TraceDecay installation targets. The wrapper always
uses the normal user-profile TraceDecay store.

The deployed `plugin_api.py` is pinned at install time only to the installing
binary path, which becomes the default `TRACEDECAY_BIN`.
`TRACEDECAY_DASHBOARD_PROJECT` or Hermes' real working directory selects the
active code project at runtime. Pass `--no-dashboard` to skip the dashboard
deploy (and remove a previous one).

To refresh the deployed page after upgrading tracedecay without touching any
Hermes configuration, run `tracedecay update-plugin`: it rewrites the
generated plugin files and dashboard page in the single user integration,
re-baking the binary path while leaving `~/.hermes/config.yaml` byte-for-byte
intact. An install created with `--no-dashboard` stays dashboard-free.

On Hermes versions that predate dashboard-plugin discovery the deployed
directory is inert — the agent-plugin loader only reads `plugin.yaml` and
ignores `dashboard/`.

Two serving modes are supported:

### 1. Spawn Mode (Default)

Hermes automatically launches the dashboard server and proxies requests to it. The server is started with `--port 0` and the URL is parsed from stdout.

**Environment variables used:**

| Variable | Required | Description |
|----------|----------|-------------|
| `TRACEDECAY_BIN` | No | Path to the tracedecay binary (defaults to the install-time binary baked into `plugin_api.py`, then `PATH`) |
| `TRACEDECAY_DASHBOARD_PROJECT` | No | Project root path (defaults to Hermes' current working directory) |

**Example:**
```bash
export TRACEDECAY_BIN=/usr/local/bin/tracedecay
export TRACEDECAY_DASHBOARD_PROJECT=/home/user/my-project
hermes dashboard
```

### 2. External URL Mode

Point Hermes at an already-running dashboard instance.

**Environment variable:**

| Variable | Required | Description |
|----------|----------|-------------|
| `TRACEDECAY_DASHBOARD_URL` | Yes | Full URL to a running tracedecay dashboard (e.g., `http://127.0.0.1:7341/`) |

**Example:**
```bash
# Terminal 1: Start dashboard
tracedecay dashboard --port 7341

# Terminal 2: Tell Hermes to use it
export TRACEDECAY_DASHBOARD_URL=http://127.0.0.1:7341/
hermes dashboard
```

When using external URL mode, the Hermes plugin acts as a reverse proxy, rewriting request paths from `/api/plugins/tracedecay/*` to the tracedecay dashboard's native paths (`/holographic`, `/lcm`, `/graph`, and `/savings` map to `/api/plugins/holographic`, `/api/plugins/hermes-lcm`, `/api/plugins/graph`, and `/api/plugins/savings` respectively).

---

## Dashboard Tabs

### Holographic Memory

The Holographic Memory tab provides interactive exploration of your project's persistent memory store.

#### Inspector

Browse and search through:
- **Facts**: Stored memories with content, category, tags, trust scores, and retrieval statistics
- **Entities**: Named concepts (functions, types, files, etc.) linked to facts
- **Memory Banks**: Per-category HRR (Holographic Reduced Representation) vector storage

Features:
- Search facts by content or tags
- Trust score histogram visualization
- HRR coverage status per category (ready, missing_vectors, missing_bank, stale_bank)
- Fact growth chart (last 30 days)
- **Fact Detail View**: Click any fact to see full untruncated content, linked entities, and trust score components

#### Semantic Map

2D PCA visualization of holographic vectors:
- Projects high-dimensional HRR vectors into an interactive 2D scatter plot
- Points colored by category
- Shows content preview and trust score on hover
- Uses dual-PCA via Gram matrix power iteration (handles up to 200 facts efficiently)

#### Association Graph

Interactive force-directed graph showing:
- **Fact nodes**: Individual memories (links to categories and entities)
- **Category nodes**: Fact categories (e.g., "architecture", "decisions")
- **Entity nodes**: Named concepts referenced by facts
- **Bank nodes**: HRR vector storage banks
- **Edges**: Contains, mentions, and bundles relationships

#### Similarity

Detects duplicate and related facts using phase-vector cosine similarity:
- Computes `mean(cos(p_i - p_j))` over all HRR phase vectors
- Classifies pairs as:
  - `likely_duplicate`: Similarity >= 0.95 with lexical overlap
  - `merge_candidate`: Similarity >= 0.90 with moderate overlap
  - `related`: Lower similarity
- Configurable threshold and pair limit
- Shows shared tokens and overlap coefficients

#### Curation

*(Availability controlled by capability flag `features.curation`)*

The Curation panel is organized into four sub-tabs:

- **Automation**: Scheduler state (with pause/resume), the effective
  automation config editor (app-server backend, standalone host, per-task schedules),
  per-task **Run** buttons for the memory curator / session reflector /
  skill writer loops, and the automation run ledger with hash-verified
  artifact drill-down.
- **Proposals**: Session-reflection fact automation outcomes and proposals
  for inspection (recent first; resolved proposals are collapsed) and managed-skill
  drafts with approve/disable/archive/restore actions. The tab label shows
  the pending count.
- **History**: Curator run history, recent snapshots, and the memory operation
  log (oplog).
- **Activity**: Live event log of autonomous curation phases and apply outcomes.

Curation is implemented as similarity-based deduplication (no LLM calls). It proposes hard-deleting the lower-trust fact in each `likely_duplicate` pair (similarity ≥ 0.95 with lexical overlap). Rule-based hygiene signals are emitted separately as `hygiene_candidates`; they are review evidence for a human or external LLM curator, not deterministic apply operations.

**Deletion is permanent — there is no archive, no restore, and no soft-delete state.** Deleted facts are removed from `memory_facts` along with their entity links (FK cascade) and FTS rows (trigger), so they immediately disappear from `tracedecay_fact_store` recall. The winner fact in a merge operation may have its content rewritten and HRR vector re-encoded.

External planners (such as an LLM-backed Hermes wrapper, gated behind the `features.llm_curation` flag) can apply their own delete/merge operations through `POST /curate/apply` (see API reference).

### LCM

The LCM (Lossless Context Management) tab visualizes agent session transcripts and summary nodes from the project's session store.

**Ingest Durability**: Transcript ingest uses per-file byte offsets and file-identity-based rewrite detection. If a session file is rewritten (different content at the same path), the offset resets automatically and the new content is fully ingested. Transactional commits ensure no data loss during concurrent ingest operations.

#### Storage Scopes — Where Messages Live

Transcript ingest is **per project**, not global:

| Store | Path | Written by | `storage_scope` |
|-------|------|------------|-----------------|
| User project store (default) | `~/.tracedecay/projects/<project-id>/sessions.db` | All transcript ingest for sessions belonging to that project root | `"profile_sharded"` |
| Global | `~/.tracedecay/global.db` | Cross-project registry (project paths, savings ledger) — **no session messages are ingested here** | `"global"` |

The dashboard serves the active project's resolved profile store by default. The LCM header shows a **"User project store"** or **"Global store"** badge. Every LCM API payload reports the active store via the additive `path` + `storage_scope` fields.

Setting `TRACEDECAY_GLOBAL_DB` pins the dashboard to an explicit store instead (used by tests and the smoke harness). When this override is active, `storage_scope` becomes `"global"`.

#### How Ingest Works Per Tool

| Tool | Trigger |
|------|---------|
| Cursor | Cursor hooks ingest incrementally at end of turn / stop / session start (subagent transcripts included); no sweep needed |
| Claude Code, Codex, Vibe, Cline / Roo / Kilo | No hooks — discovered by a catch-up sweep that scans each tool's home transcript directory (e.g. `~/.codex/sessions`) and ingests sessions whose recorded `cwd`/project matches the served project root |
| Hermes | Hermes-side ingest into the same resolved user-level project store as the generated TraceDecay adapter; Hermes profile directories are transcript inputs, never project identities |

The catch-up sweep runs automatically when the MCP server starts
(`tracedecay serve`) and when `tracedecay dashboard` starts with project-local
scope. Ingest is incremental (per-file byte offsets in `parse_offsets`), so
repeat sweeps are cheap no-ops.

#### Overview

Summary statistics and recent activity:
- Total messages and sessions
- Summary node counts and compression ratios
- Role distribution (user, assistant, system, tool)
- Source/provider breakdown
- Summary depth distribution. Codex compaction summaries use this depth as
  compaction generation; ordinary LCM condensation summaries use DAG lineage
  depth.
- Recent sessions with message counts
- Recent summary nodes

#### Search

Full-text search across:
- Raw messages (`lcm_raw_messages` table)
- Summary nodes (`lcm_summary_nodes` table)

Facets/filters:
- **Role**: Filter by message role (user, assistant, system, tool)
- **Source**: Filter by provider/source
- **Session ID**: Filter to a specific session
- **Time range**: Since/until (epoch timestamps)

Search engines (automatic fallback):
- **FTS5**: Fast full-text search when FTS tables are available
- **LIKE**: Pattern matching fallback with snippet extraction

#### Session Detail

Drill into individual sessions:
- Complete message list with pagination
- Associated summary nodes (hierarchical LCM structure)
- Token estimates and metadata
- Chronological or reverse-chronological ordering

#### Node Detail

Expand summary nodes to see:
- Node metadata (depth, category, compression ratio)
- Source items: either raw messages or child summary nodes
- Lossless reconstruction of the summarized content

#### Timeline

Time-bucketed activity visualization:
- **Hourly** or **daily** buckets
- Message counts per bucket
- Summary node counts per bucket
- Filterable by session ID

#### Compression

Analyze LCM compression efficiency:
- Overall compression ratio (source tokens → summary tokens)
- Per-session breakdown
- Per-node breakdown
- Node count and token savings statistics

#### Store Maintenance

The overview footer surfaces external-payload health from
`GET /payloads/health` (externalized payload count and bytes, reclaimable
bytes, orphan files, missing payloads/placeholder refs, tombstones, and the
last GC outcome) with a two-step payload GC flow: **Preview GC** runs the
dry-run (`GET /payloads/gc`) and **Apply GC** submits the returned
`dry_run_token` (`POST /payloads/gc`) to reap unreferenced payload files.
Stores that never externalized a payload report an empty "nothing to
reclaim" dry run rather than an error.

### Code Graph

The Code Graph tab is an interactive explorer over the project's indexed code
graph (`nodes`, `edges`, `files` in `.tracedecay/tracedecay.db`).

- **Overview**: orientation analytics — symbols by kind family, files by
  language, most-connected symbols, largest files, and an edge-kind strip.
  Chart rows are clickable and open the canvas pre-filtered or focused.
- **Canvas**: a force-directed canvas-2D explorer with search-to-focus,
  progressive neighbor expansion (double-click or Inspector buttons), kind /
  language / directory-scope filters, callers/callees drilldown, and a
  **Find path** mode that highlights the shortest path between two symbols.

The backend routes live under `/api/plugins/graph/*` (proxied by the Hermes
wrapper at `/api/plugins/tracedecay/graph/*`). See
[graph-explorer.md](graph-explorer.md) for the full API table, frontend
design, and performance notes.

### Savings & Cost

The Savings & Cost tab is the accounting surface: how many tokens tracedecay
saved you, and what your agent sessions cost. Three views behind a shared
time-range selector (All time / Today / 7 days / 30 days):

- **Savings**: the `savings_ledger` event log from the global accounting DB
  (`~/.tracedecay/global.db`, the same data `tracedecay gain` reports) —
  totals, per-tool and per-project breakdowns, a daily series, and the legacy
  per-project lifetime counters (`projects.tokens_saved`), which predate the
  ledger and usually carry the big historical numbers. Saved tokens are
  valued in dollars at the Claude Sonnet *input* rate (same convention as
  `tracedecay gain`), labeled as estimated. The view discloses the
  methodology inline: per call, `before` = indexed bytes/4 of every file the
  response references (full-read counterfactual), `after` = response
  chars/4, saved = `max(0, before - after)` — an estimated upper bound,
  since repeated calls re-count files and agents would not always have read
  every referenced file raw. (Historical lifetime counters accumulated the
  gross `before` without subtracting responses; the recording path now
  credits only the net difference.)
- **Sessions**: one row per ingested session from the session store (the
  same store the LCM tab serves) — model(s) used, input/output token counts,
  cost, and a **cost basis** badge. Rows expand to a per-model breakdown
  with the resolved OpenRouter slug.
- **Models & Pricing**: aggregate cost per model and per day, the `turns`
  accounting imported by `tracedecay cost` from Claude Code transcripts
  (always `actual` — costs were computed from real usage data at ingest),
  and a panel showing where prices came from.

#### Cost-basis semantics (three quality tiers)

Every token count and cost is labeled with its provenance — in the UI
(badges) and in the API (`cost_basis` fields). The best available tier wins
per message:

- **`actual`** — the transcript recorded real usage data
  (`metadata_json.usage.input_tokens`/`output_tokens`, or OpenAI-style
  `prompt_tokens`/`completion_tokens`; cache read/write tokens are honored
  too). Costs computed from these are labeled *actual (from transcript
  usage)*. Claude transcripts carry Anthropic usage verbatim; Codex
  `token_count` events are normalized at ingest (cached input split into
  `cache_read_input_tokens`).
- **`tokenized`** — no usage data, but the stored message text was counted
  with a real BPE tokenizer (tiktoken). Exact for OpenAI-family models
  (`o200k_base` for GPT-5/4o/4.1/o-series/codex/gpt-oss, `cl100k_base` for
  legacy GPT-4/GPT-3.5/embeddings); for vendors without a public tokenizer
  (Claude, Gemini, Grok, …) `o200k_base` serves as a much-better-than-chars/4
  approximation, marked `≈` in the UI and `"exact": false` in the API's
  per-row `tokenizer` block. This is the primary tier for Cursor (whose
  transcripts carry **no** token counters at all), cline, and vibe stores.
  Counts are cached per message for the lifetime of the dashboard process.
  The cache is derived acceleration and is never persisted as an independent
  storage authority; a background warm task runs at dashboard startup. Built
  behind the `token-counting` cargo feature (on by default; ~4 MB of embedded
  vocabularies, decoded lazily on first use).
- **`estimated`** — the fallback ~4 chars/token heuristic the LCM views use
  (`(LENGTH(text)+3)/4`), attributing non-assistant text to input and
  assistant text to output. Applies when the binary was built without
  `token-counting` (or a message has no countable text). All non-usage
  tiers only cover stored message text — resent context windows and tool
  payloads are not modeled — so those costs are a deliberate lower bound,
  and the UI says so.
- **`mixed`** — a session/aggregate containing both usage-backed and
  non-usage messages (unchanged legacy meaning).

The three tiers never overlap in the API: per row, `actual` + `tokenized` +
`estimated` token blocks partition the messages, and `tokenized_messages` /
`estimated_messages` count the non-usage split.

Messages with no recorded model id appear as explicit **unknown model** rows:
their tokens are counted but never priced.

#### Ledger recording

MCP servers append a `savings_ledger` row after every tool call **by
default** whenever the global accounting DB is available. Opt out with
`TRACEDECAY_DISABLE_GLOBAL_DB=1` (or `TRACEDECAY_ENABLE_GLOBAL_DB=0`); an
explicit `TRACEDECAY_ENABLE_GLOBAL_DB=1` always wins (it is what
`tracedecay install` writes for user-global agent configs, and what tests
use to opt back in past the repo's cargo-test opt-out). The Savings view
surfaces the gate verdict (`recording: on/off` badge plus an explanatory
note when the ledger is empty), and the overview API reports it under
`savings.recording` (`{"enabled": bool, "mode": "default" |
"enabled_by_env" | "disabled_by_env"}`). Note that a long-running MCP
server evaluates the gate at startup — restart/reload your agent's
tracedecay server after changing the environment (or after upgrading from a
build that defaulted the ledger off).

#### Model pricing

Prices come from [OpenRouter's public model list](https://openrouter.ai/api/v1/models)
(no auth needed for pricing metadata):

1. **Disk cache** at `~/.tracedecay/model-prices.json` (override:
   `TRACEDECAY_MODEL_PRICES_PATH`) — served immediately, even when stale.
2. **Background refresh** at most once per process when the cache is older
   than 24h. The fetch never blocks a request and never fails the dashboard.
3. **Bundled snapshot** (`src/dashboard/model_prices_fallback.json`, a
   curated ~157-model subset) — used when there is no usable cache, so the
   tab works offline and on first run.

`TRACEDECAY_OFFLINE=1` disables the network entirely (cache/snapshot only).
Transcript model ids are fuzzy-mapped to OpenRouter slugs client-side
(`dashboard/savings/src/pricing.ts`): manual alias table, effort/thinking
suffix stripping (`claude-fable-5-thinking-xhigh` → `anthropic/claude-fable-5`),
dash→dot version normalization (`claude-opus-4-8` → `claude-opus-4.8`),
Claude family/version reorder (`claude-4.6-sonnet` → `claude-sonnet-4.6`),
and vendor-prefix probing. Unmatched models (e.g. Cursor's
`composer-2.5-fast`) show *no price data* — the UI never guesses.

---

## API Reference

All API endpoints return JSON. The dashboard mirrors the original Hermes plugin API paths for compatibility.

### Error Responses

All error responses use a consistent JSON contract with an HTTP 4xx status code and a `detail` field:

```json
{
  "detail": "Human-readable error message"
}
```

Common status codes:
- `400` — Bad request (invalid query parameters, malformed input)
- `404` — Resource not found (unknown fact ID, missing node, non-existent session)
- `422` — Unprocessable entity (validation errors, semantic constraints violated)

Example: Requesting a non-existent fact returns `404` with `{"detail": "fact not found: 12345"}`. Invalid query parameters (e.g., `limit=not-a-number`) return `400` with details about the parameter.

### Capability Discovery

#### `GET /api/capabilities`

Returns feature flags and server configuration. Used by the UI and wrappers to determine which panels/actions to enable.

**Response:**
```json
{
  "name": "tracedecay-dashboard",
  "version": "0.0.2",
  "mode": "standalone",
  "project_root": "/home/user/my-project",
  "memory_db": "/home/user/.tracedecay/projects/proj_1234/tracedecay.db",
  "lcm_db": "/home/user/.tracedecay/projects/proj_1234/sessions.db",
  "lcm_scope": "profile_sharded",
  "features": {
    "memory": true,
    "lcm": true,
    "graph": true,
    "curation": true,
    "automation": true,
    "llm_curation": true,
    "managed_skills": true
  },
  "automation": {
    "enabled": true,
    "mode": "standalone_backend",
    "backend": "codex_app_server",
    "host_mode": "standalone",
    "availability": {"available": true, "reason": ""}
  },
  "dashboards": ["holographic", "hermes-lcm", "graph"]
}
```

**Fields:**
- `mode`: `"standalone"` for direct use, `"hermes"` when wrapped by Hermes
- `lcm_db` / `lcm_scope`: The LCM session store being served and its scope (`"profile_sharded"`, `"project_local"`, or `"global"`; see [Storage Scopes](#storage-scopes--where-messages-live))
- `features.memory`: Whether the project database is available
- `features.lcm`: Whether the LCM session store is available
- `features.curation`: Whether similarity-dedup curation tools are enabled
- `features.automation`: Whether TraceDecay automation is enabled with a supported backend
- `features.llm_curation`: Whether TraceDecay can run LLM-backed curation through standalone automation. Delegated hosts keep planning host-owned and submit ops through `POST /curate/apply`.
- `automation.mode`: `"disabled"`, `"standalone_backend"`, or `"delegated_host"`; `delegated_host` is provider-neutral and may be used by Hermes, Codex app-server orchestration, Claude Code CLI, Cursor Agent CLI, or another host that owns the intelligence layer.

### Automation Scheduler Debugging

The dashboard scheduler panel reads `GET /api/automation/scheduler/status` and can pause or resume the scheduler with `/pause` and `/resume`. The status response includes the effective automation config, control file path, tick cadence, and per-task due/skip reasons.

When the daemon runs the scheduler, its stderr/journald logs use stable `event=... key=value` fields:

```bash
tracedecay daemon status
journalctl --user -u tracedecay.service -f
journalctl --user -u tracedecay.service --since "1 hour ago" | grep 'event=scheduler'
```

Useful events include `event=scheduler_tick`, `event=scheduler_sleep`, `event=scheduler_task`, and `event=scheduler_task_error`.

---

### Holographic Memory API

Base path: `/api/plugins/holographic`

#### `GET /api/plugins/holographic/`

Main overview endpoint returning facts, entities, and graph data.

**Query Parameters:**
- `q` — Search query for fact content/tags
- `limit` — Max facts/entities to return (default: 25, max: 100)
- `graph_limit` — Max graph nodes (default: same as `limit`, max: 1000)

**Response Structure:**
```json
{
  "providers": { /* provider metadata */ },
  "query": "",
  "limit": 25,
  "holographic": {
    "path": "/path/to/tracedecay.db",
    "exists": true,
    "overview": {
      "facts": 133,
      "entities": 685,
      "banks": 6,
      "categories": [...],
      "entity_types": [...],
      "hrr_coverage": [...],
      "trust_histogram": [...],
      "growth": [...]
    },
    "facts": [...],
    "entities": [...],
    "graph": { "nodes": [...], "edges": [...] }
  }
}
```

#### `GET /api/plugins/holographic/fact/{fact_id}`

Full fact detail. List and projection payloads truncate `content` to 200
characters; detail panels (e.g. the Semantic Map's pinned card) fetch the
complete row — plus linked entities — from here. Returns `404` with a
`{"detail": "fact not found: <id>"}` body for unknown ids.

**Response:**
```json
{
  "fact": {
    "fact_id": 103,
    "content": "Full untruncated fact content…",
    "category": "tool",
    "tags": "[\"lcm\",\"ux\"]",
    "trust_score": 0.76,
    "retrieval_count": 3,
    "access_count": 1,
    "last_recalled_at": 1700000150,
    "helpful_count": 2,
    "created_at": 1700000020,
    "updated_at": 1700000120,
    "has_hrr": 1,
    "entities": [
      { "entity_id": 202, "name": "LCMTab", "entity_type": "feature" }
    ]
  },
  "error": ""
}
```

`access_count` / `last_recalled_at` track only recall-search returns
(`fact_store` `action: "search"` results actually handed to a caller);
`retrieval_count` also counts probe/list/related/reason scans. Access
frequency deliberately does NOT feed recall ranking (rich-get-richer risk) —
it is a curation signal (delete-reluctance for actively used facts).

#### `GET /api/plugins/holographic/projection`

2D PCA projection of HRR vectors for the Semantic Map visualization.

**Query Parameters:**
- `q` — Filter facts by search query
- `limit` — Max facts to project (default: 25, max: 200)

**Response:**
```json
{
  "exists": true,
  "dim": 2048,
  "method": "pca",
  "points": [
    {
      "fact_id": 1,
      "x": 0.123456,
      "y": -0.654321,
      "category": "architecture",
      "content": "Fact content preview...",
      "trust_score": 0.95,
      "retrieval_count": 42
    }
  ],
  "error": ""
}
```

#### `GET /api/plugins/holographic/similarity`

Pairwise similarity analysis for duplicate detection.

**Query Parameters:**
- `threshold` — Minimum similarity score (default: 0.85)
- `limit` — Max pairs to return (default: 25, max: 200)

**Response:**
```json
{
  "exists": true,
  "dim": 2048,
  "count": 50,
  "threshold": 0.85,
  "pairs": [
    {
      "a_id": 1,
      "b_id": 2,
      "a_content": "First fact content...",
      "b_content": "Second fact content...",
      "a_category": "architecture",
      "b_category": "architecture",
      "similarity": 0.96,
      "classification": "likely_duplicate",
      "token_overlap": 0.45,
      "overlap_coefficient": 0.65,
      "shared_tokens": ["token1", "token2"]
    }
  ],
  "error": ""
}
```

**Classification rules:**
- `likely_duplicate`: Similarity >= 0.95 AND (overlap_coefficient >= 0.65 OR token_overlap >= 0.45)
- `merge_candidate`: Similarity >= 0.90 AND (overlap_coefficient >= 0.35 OR token_overlap >= 0.20)
- `related`: Lower similarity values

#### `GET /api/plugins/holographic/curation/status`

Curation system status and configuration.

#### `GET /api/plugins/holographic/curation/activity`

Recent curation activity log.

#### `POST /api/automation/run/memory-curator`

Queue an autonomous app-server memory-curator run. The dashboard does not
expose a saved human review form; accepted curation operations are validated
and applied by the automation policy, with each phase recorded in the run
ledger, artifacts, telemetry, and curation activity stream.

#### `POST /api/plugins/holographic/curate/apply`

Generic curation-ops apply endpoint. Standalone automation backends and
delegated host planners use this contract. Per-op failures are
reported per-op in `results`; the request only fails wholesale (400) on a
malformed body.

**Request Body:**
```json
{
  "ops": [
    { "op": "delete", "fact_id": 5, "reason": "stale duplicate" },
    {
      "op": "merge",
      "winner_id": 3,
      "loser_ids": [5, 9],
      "merged_content": "Optional combined fact text"
    }
  ]
}
```

- `delete` — hard-deletes the fact (entity links cascade, FTS rows drop).
- `merge` — optionally rewrites the winner's content with `merged_content`
  (re-encodes the HRR vector and entity links), then hard-deletes the losers.
  The winner is validated before any mutation; a missing winner fails the op
  and leaves the losers untouched.

**Response:**
```json
{
  "results": [
    { "op": "delete", "fact_id": 5, "reason": "stale duplicate", "status": "deleted" },
    {
      "op": "merge",
      "winner_id": 3,
      "content_updated": true,
      "deleted_loser_ids": [9],
      "failed_losers": [],
      "status": "merged"
    }
  ],
  "counts": { "deleted": 1, "merged": 1, "errors": 0 }
}
```

Failed ops carry `"status": "error"` and an `"error"` message (e.g.
`fact 99999 not found`, `unsupported op 'x'`, `winner fact 42 not found`).

#### `GET /api/plugins/holographic/oplog`

Recent memory operations, newest first, from `memory_oplog` — the append-only
audit written by the store mutation paths (`add` / `update` / `remove` /
`feedback`, plus `reject_secret_like` for blocked writes) and curation applies
(`curate_apply`). `detail` never carries fact content beyond what the op
needs; deletes record a `content_hash`, not the content (the hard-delete
stance is preserved).

**Query Parameters:**
- `limit` — Max rows (default: 50, max: 300)

**Response:**
```json
{
  "events": [
    { "id": 12, "ts": 1765000000, "op": "curate_apply", "fact_id": null,
      "detail": { "mode": "ops", "deleted": 1, "merged": 0, "errors": 0 } },
    { "id": 11, "ts": 1765000000, "op": "remove", "fact_id": 103,
      "detail": { "category": "tool", "content_hash": "9f2c..." } }
  ],
  "count": 2,
  "limit": 50,
  "error": ""
}
```

---

### LCM API

Base path: `/api/plugins/hermes-lcm`

#### `GET /api/plugins/hermes-lcm/overview`

Summary statistics and recent sessions/nodes.

**Query Parameters:**
- `q` — Search query (returns matches alongside overview)
- `limit` — Max recent sessions/nodes (default: 25, max: 200)

**Response Structure:**
```json
{
  "path": "/home/user/.tracedecay/projects/proj_1234/sessions.db",
  "storage_scope": "profile_sharded",
  "exists": true,
  "overview": {
    "messages_total": 1500,
    "sessions_total": 25,
    "summary_nodes_total": 150,
    "summary_node_sessions_total": 20,
    "max_summary_depth": 3,
    "role_counts": [{"role": "user", "count": 800}, ...],
    "source_counts": [{"source": "claude", "count": 1500}, ...],
    "depth_counts": [{"depth": 0, "count": 100}, ...],
    "compression": {
      "source_token_count": 50000,
      "token_count": 5000,
      "ratio": 10.0,
      "node_count": 150
    }
  },
  "latest_sessions": [...],
  "latest_summary_nodes": [...],
  "matches": { "messages": [], "summary_nodes": [] },
  "query": "",
  "limit": 25
}
```

#### `GET /api/plugins/hermes-lcm/search`

Full-text search with facets.

**Query Parameters:**
- `q` — Search query (required)
- `limit` — Max results per type (default: 25, max: 200)
- `role` — Filter by message role
- `source` — Filter by provider/source
- `session_id` — Filter to specific session
- `since` — Epoch timestamp (inclusive)
- `until` — Epoch timestamp (inclusive)

**Response:**
```json
{
  "path": "/home/user/.tracedecay/projects/proj_1234/sessions.db",
  "storage_scope": "profile_sharded",
  "exists": true,
  "query": "authentication",
  "limit": 25,
  "engine": "fts",
  "filters": {
    "role": null,
    "source": null,
    "session_id": null,
    "since": null,
    "until": null
  },
  "matches": {
    "messages": [
      {
        "store_id": 123,
        "session_id": "sess-abc",
        "role": "user",
        "source": "claude",
        "timestamp": 1700000000,
        "token_estimate": 25,
        "content": "How does authentication work?",
        "snippet": "How does [authentication] work?"
      }
    ],
    "summary_nodes": [...]
  }
}
```

#### `GET /api/plugins/hermes-lcm/session/{session_id}`

Get all messages and summary nodes for a session.

**Query Parameters:**
- `limit` — Max messages (default: 200, max: 1000)
- `offset` — Pagination offset
- `order` — `"asc"` or `"desc"` (default: `"desc"`)

#### `GET /api/plugins/hermes-lcm/node/{node_id}`

Get a summary node with its source items.

**Response:**
```json
{
  "path": "/home/user/.tracedecay/projects/proj_abc/sessions.db",
  "storage_scope": "profile_sharded",
  "exists": true,
  "node_id": "node-abc",
  "node": { /* node details */ },
  "sources": {
    "type": "messages",
    "ids": [1, 2, 3],
    "messages": [...],
    "nodes": []
  }
}
```

#### `GET /api/plugins/hermes-lcm/timeline`

Time-bucketed activity counts.

**Query Parameters:**
- `bucket` — `"hour"` or `"day"` (default: `"day"`)
- `session_id` — Filter to specific session (optional)
- `limit` — Max buckets (default: 400, max: 2000)

#### `GET /api/plugins/hermes-lcm/compression`

Compression statistics.

**Query Parameters:**
- `by` — Group by `"session"` or `"node"` (default: `"session"`)
- `limit` — Max groups (default: 50, max: 500)

### Savings & Cost API

Routes under `/api/plugins/savings/*` (proxied by the Hermes wrapper at
`/api/plugins/tracedecay/savings/*`). All endpoints degrade
gracefully: when a backing store is unavailable they return `200` with
`"available": false` instead of failing. `range` accepts `today`, `7d`,
`30d`, `all` (default `all`; sessions without any timestamp — e.g. Cursor
hook ingests — only appear in `all`).

#### `GET /api/plugins/savings/overview`

Combined summary: ledger totals (today / 7d / 30d / all-time), the
ledger-recording gate verdict (`savings.recording`), lifetime per-project
counters, session-store rollup (message counts split into `usage_messages`
/ `tokenized_messages` / `estimated_messages`, token sums per tier,
`unknown_model_messages`, `token_counting` build flag), `turns` accounting
totals, and pricing provenance (`source`, `fetched_at`, `offline`).

#### `GET /api/plugins/savings/ledger`

Savings-ledger detail for a range: `total`, `by_day`, `by_tool`,
`by_project`. Reuses the same aggregation as `tracedecay gain` / `--history`.

**Query Parameters:** `range`

#### `GET /api/plugins/savings/sessions`

Paged per-session cost rows. Each session carries `cost_basis`
(`"actual" | "tokenized" | "estimated" | "mixed"`), `usage_messages` /
`tokenized_messages` / `estimated_messages`, and a `models` array; each
model entry has `model` (`null` = unknown model), its own `cost_basis`, a
`tokenizer` block (`{"encoder", "exact"}`, `null` when the build lacks
`token-counting`), an `actual` block (`input_tokens`, `output_tokens`,
`cache_read_tokens`, `cache_write_tokens`), a `tokenized` block and an
`estimated` block (`input_tokens`, `output_tokens` each; the three blocks
never overlap). Dollar costs are computed by the UI from the `/pricing`
table.

**Query Parameters:** `range`, `limit` (default 25, max 100), `offset`

#### `GET /api/plugins/savings/models`

Per-model aggregates (same token-block shape as session model entries, plus
`sessions`), a `daily` series for timestamped messages, and the `turns`
block: `by_model` (`model`, `cost_usd`, `total_tokens`, `cost_basis:
"actual"`) and `by_day` — reusing the `tracedecay cost` queries.

**Query Parameters:** `range`

#### `GET /api/plugins/savings/pricing`

The merged model price table: `source` (`"cache"` or `"fallback"`),
`fetched_at` (cache mtime), `ttl_secs`, `offline`, `cache_path`,
`model_count`, and `models` — OpenRouter slug → `prompt_per_mtok`,
`completion_per_mtok`, `cache_read_per_mtok`, `cache_write_per_mtok` (USD
per million tokens). Requesting this endpoint (or `/overview`) kicks off the
at-most-once background refresh when the cache is stale and
`TRACEDECAY_OFFLINE` is unset.

### Settings API

Routes under `/api/settings` back the Settings tab.

#### `GET /api/settings`

Aggregated settings payload: `project` (the pinned runtime configuration —
include/exclude globs, `max_file_size`, `extract_docstrings`,
`track_call_sites`, `git_ignore`, `telemetry.timings`, and
`sync.auto_track_pr_branches` / `sync.auto_track_pr_poll_secs`; the legacy
`config.json` path is exposed as read-only), `user` (editable user-level
settings from `~/.tracedecay/config.toml`: `upload_enabled`,
`watcher_debounce`, `extraction_timeout_secs`), `automation` (read-only summary
linking to the existing editor at
`/api/plugins/holographic/curation/config`),
`environment` (read-only env-var gate verdicts with explanations),
`storage` (resolved store paths), and `version` (version + update channel).
The project object carries `configuration_revision_id`; the user object carries
the independent content-derived `user_settings_revision_id`.

#### `PATCH /api/settings/project`

Partial update of the project runtime configuration, including the telemetry
and PR-auto-track fields listed above. It validates glob patterns,
`max_file_size >= 1`, and the minimum PR polling interval, and rejects unknown
fields. Errors follow the automation config contract (`validation_errors`
array with `field` + `message`). The response includes
`resync_recommended: true` when the project configuration changed — the
endpoint never auto-runs a sync. Every request must include the
`expected_revision_id` returned as `project.configuration_revision_id` by GET;
this project-resource revision is independent of the user-settings revision.
A stale revision returns HTTP 409:

```json
{
  "code": "configuration_revision_conflict",
  "detail": "settings changed after this edit began; refresh and retry",
  "expected_revision_id": "configuration.revision.old",
  "actual_revision_id": "configuration.revision.current"
}
```

#### `PATCH /api/settings/user`

Partial update of the editable user-level settings. Validates
`watcher_debounce` durations (`"2s"`, `"1m"`, …) and
`extraction_timeout_secs >= 1`. The response includes
`restart_recommended: true` when a startup-read knob changed. Every request
must include the `expected_revision_id` returned as
`user.user_settings_revision_id` by GET. The revision is derived from the
persisted user config and checked under the same lock as the atomic write, so
concurrent writers cannot both commit. A stale revision returns HTTP 409:

```json
{
  "code": "configuration_revision_conflict",
  "detail": "user settings changed after this edit began; refresh and retry",
  "expected_revision_id": "sha256:old",
  "actual_revision_id": "sha256:current"
}
```

---

## Capability Flags

The dashboard uses capability flags to advertise which features are live. The UI checks these flags to decide which panels to show and which actions to enable.

### Client-Side Detection

JavaScript example:
```javascript
fetch('/api/capabilities')
  .then(r => r.json())
  .then(capabilities => {
    if (capabilities.features.curation) {
      showCurationPanel();
    }
    if (capabilities.features.llm_curation) {
      enableLlmPlannerActions();
    }
  });
```

### Flag Semantics

| Flag | Meaning | UI Impact |
|------|---------|-----------|
| `features.memory` | Project database is accessible | Show Holographic Memory tab |
| `features.lcm` | LCM session store is accessible (see `lcm_scope` for which one) | Show LCM tab |
| `features.graph` | Code-graph API is available | Show Code Graph tab |
| `features.savings` | Savings & Cost API is available | Show Savings & Cost tab |
| `features.settings` | Aggregated Settings API is available | Show Settings tab |
| `features.curation` | Similarity-dedup curation tools are available | Show Curation panel, enable curate actions |
| `features.llm_curation` | LLM-backed curation is available through standalone automation | Show autonomous curation status and run history |

There is no archive flag: curation deletes are permanent, and no archive or
restore endpoints exist. Always check the capability flags rather than
assuming availability — they may change based on database state and host mode
(standalone backend vs delegated host).

---

## Frontend Development

The dashboard frontend source lives in `dashboard/`:

| Directory | Contents |
|-----------|----------|
| `dashboard/src/app/` | Single-app entry point, router, and product shell |
| `dashboard/src/workspaces/` | The twelve product workspaces |
| `dashboard/src/ui/` | Shared states, controls, and workspace archetypes |
| `dashboard/src/data/` | API queries, scope, and event-stream clients |
| `dashboard/src/viz/` | Shared chart and graph renderers |
| `dashboard/stories/` | Fixture-backed visual and accessibility audits |
| `dashboard/codegen/` | Rust-contract-to-TypeScript generation and checks |
| `dashboard/app-dist/` | Rsbuild output embedded and served at `/` — generated, git-ignored, never committed |

### Building

```bash
cd dashboard
npm ci
npm run typecheck
npm run contracts:check
npm test
npm run build
```

`dashboard/app-dist/` is listed in the repository `.gitignore` and is not
tracked. CI builds it once in the `dashboard-assets` job and every Rust job
downloads it as an artifact; `Cargo.toml`'s `package.include` whitelist still
ships `dashboard/app-dist/**` in the published crate, so the release workflow
must build the bundle before `cargo package`. Do not commit build output.

### Wire contracts

`dashboard/src/contracts/generated.ts`, `dashboard/src/contracts/index.ts`, and
`dashboard/codegen/schemas/dashboard-contracts.schema.json` are generated from
Rust `schemars` output and are the only Rust-to-dashboard wire boundary.
`npm run contracts:check` re-exports the schema through `cargo test --test
dashboard_contract_schema_export -- --ignored writes_dashboard_contract_schema`,
regenerates all three files, and exits non-zero if any committed file differs.
It is a blocking step of the `Dashboard integration` CI job, not an advisory
comparison against a preview artifact. Regenerate with `npm run
contracts:generate` and commit the result; never hand-edit the outputs.

The typed graph-structure routes are registered from the same declarations that
name their response schemas. Schema export asserts every one of those registered
responses exists in the contract catalog before writing generated output. A new
typed route with no catalog entry therefore fails `contracts:check` with its
method, path, and missing response type. Legacy type-erased plugin routes are
not represented as fake `serde_json::Value` contracts.

`npm run build` invokes Rsbuild using `dashboard/rsbuild.config.ts`, with
`dashboard/src/app/main.tsx` as the entry point and `dashboard/app-dist/` as
the output. The legacy `dashboard/{shell,holographic,lcm,graph,...}/dist`
bundles are compatibility assets for `/legacy` and the Hermes wrapper; they are
not the production `/` application.

### Frontend verification

Vitest covers workspace models and DOM behavior. The Playwright visual audit
walks the story registry at 320, 768, and 1440 pixel widths in both themes and
records axe results. A separate accessibility gate drives the built bundle
through the states a plain navigation never reaches and fails the process on
any violation, page error, or failed assertion:

```bash
cd dashboard
npm test
npm run visual:audit      # screenshots + per-surface axe, gallery output
npm run axe:audit         # the accessibility gate; non-zero exit on violations
npm run axe:explorer      # explorer-only scan on its own port and output dir
```

The axe engine is one file, `dashboard/e2e/axe-harness.ts`; scenarios live in
`dashboard/e2e/axe-audit.ts`. The earlier per-lane copies (including
`e2e/axe-governance.ts` and the `.explorer-axe/` and `.governance-axe/`
dot-directory forks) were deleted after two of them ended in an unconditional
`process.exit(0)` and reported violations while still exiting clean.

These frontend checks do not verify the Rust routes. The aggregate Rust
`dashboard_api_test` suite is currently unverified as noted at the top of this
document.

### Asset Embedding

The shipped binary has no Node or Rsbuild dependency at launch. `build.rs`
generates a Rust manifest for every file under `dashboard/app-dist/`, and
`src/dashboard/assets.rs` serves that embedded manifest at `/` and
`/static/**`. The old shell and plugin bundles remain separately embedded for
`/legacy` and compatibility wrappers.

After rebuilding the frontend you must rebuild the Rust binary to pick up the
new assets:

```bash
cd dashboard && npm run build
cd .. && cargo build --bin tracedecay
```

The `build.rs` script emits `cargo::rerun-if-changed` directives for all embedded assets, so the binary automatically rebuilds when dist files change.

When `app-dist` is missing or stale in a source checkout, `build.rs` runs
`npm ci` when needed and then `npm run build`. Packaged crates contain the
prebuilt app and do not include the frontend source, so registry installs do
not invoke npm.

### Packaging / crates.io

`Cargo.toml` uses an explicit `package.include` whitelist that ships the
prebuilt `dashboard/app-dist` bundle and the legacy compatibility assets. This
means:

- `cargo package` / `cargo publish` must be run after `cd dashboard && npm ci
  && npm run build` (the release workflow does this); the package verify step
  then compiles without touching npm.
- Crates.io consumers (`cargo install tracedecay`) and docs.rs need **no**
  Node.js toolchain — the embedded assets come straight from the package.

### Development Workflow

For fast frontend iteration use the dev server (HMR, no Rust rebuild):

```bash
# Terminal 1: run the API on the proxy target from rsbuild.config.ts
tracedecay dashboard --port 8321

# Terminal 2: run the single-app Rsbuild dev server
cd dashboard && npm run dev          # http://127.0.0.1:5173/
```

`dashboard/rsbuild.config.ts` owns the dev-server configuration. Set
`TRACEDECAY_DASHBOARD_API` to override its default API target.

To validate the production build (the shipped UI is always embedded bytes):

```bash
cd dashboard && npm run build        # Rsbuild → app-dist/
cd .. && cargo run -- dashboard      # rebuild Rust to embed the new assets
```

---

## Troubleshooting

### Port Already in Use

```bash
# Error: failed to bind 127.0.0.1:7341: Address already in use

# Option 1: Use a different port
tracedecay dashboard --port 8080

# Option 2: Let the OS pick a free port
tracedecay dashboard --port 0

# Option 3: Find and stop the existing process
lsof -i :7341
kill <PID>
```

### Missing Project Database

```bash
# Dashboard starts but Holographic Memory tab shows empty/error

# Ensure you've initialized tracedecay in your project
cd /path/to/project
tracedecay init
tracedecay sync

# Then restart the dashboard
tracedecay dashboard
```

### Missing LCM Data

```bash
# LCM tab shows empty state

# Session messages live in the PROJECT store, not the global DB.
# The project store is populated by:
# - Cursor transcript ingestion (via end-of-turn hooks)
# - The catch-up sweep for Claude/Codex/Vibe/Cline transcripts, which runs
#   when `tracedecay serve` or `tracedecay dashboard` starts
# - Explicit LCM tool calls

# Check which project session store is active
tracedecay status --json

# The LCM header shows which store is being served ("Project store" /
# "Global store") and its path. If it shows the global DB unexpectedly,
# check whether TRACEDECAY_GLOBAL_DB is set — it pins the store:
echo "$TRACEDECAY_GLOBAL_DB"

# Pin to an explicit store if needed
export TRACEDECAY_GLOBAL_DB=/path/to/sessions.db
tracedecay dashboard
```

### Automation Scheduler Not Running

```bash
# Check effective automation config and backend availability
tracedecay automation config explain --json

# Check run history for memory_curator/session_reflector/skill_writer
tracedecay automation runs list --json

# Check daemon socket/service/log path
tracedecay daemon status
journalctl --user -u tracedecay.service --since "1 hour ago" | grep 'event=scheduler'
```

If the config shows `enabled: false`, `backend: "disabled"`, `host_mode: "delegated_host"`, or task schedules set to `manual`, the daemon scheduler will skip work. The scheduler status panel shows the same skip reasons without requiring shell access.

### Frontend Assets Not Updating

```bash
# After editing dashboard/ source files, changes don't appear

# The dashboard serves assets embedded at compile time.
# You must rebuild both frontend and Rust:

cd dashboard && npm run build
cd .. && cargo build --bin tracedecay
```

### Build Errors: Dashboard Assets Missing

```bash
# Error: failed to run npm run build: No such file or directory (os error 2)

# app-dist is git-ignored, so a fresh clone has none and build.rs must build
# it. That panic means npm is not on PATH; install Node.js 22+, then either
# rebuild or build the frontend manually first:
cd dashboard && npm ci && npm run build
cd .. && cargo build --bin tracedecay

# Error: dashboard/app-dist/index.html is missing after build
# TRACEDECAY_SKIP_DASHBOARD_BUILD only skips a *rebuild*. In a checkout with
# no app-dist at all it skips the build and then trips this assertion. Unset
# it and let build.rs run npm, or build the frontend manually as above.
```

### Hermes Wrapper Connection Failed

```bash
# Hermes shows "Connection refused" or timeout

# Check that TRACEDECAY_BIN is correct
export TRACEDECAY_BIN=$(which tracedecay)

# For external URL mode, verify the server is running
curl http://127.0.0.1:7341/api/capabilities

# Check the Hermes plugin logs for spawn errors
```

### Slow Initial Load

The dashboard may be slow on first load if:
- The project database is very large (millions of nodes)
- The global database is on a network filesystem

Mitigations:
- Run `tracedecay sync` before starting the dashboard
- Ensure `~/.tracedecay/global.db` is on local storage
- Use `--port 0` to avoid port scanning delays

### Stale HRR Coverage Data

If the Semantic Map shows "stale_bank" status for categories:

```bash
# The bank's fact_count doesn't match current active facts.
# This is a display issue; the HRR vectors are still valid.
# The status will refresh on next memory bank update.
```

---

## Architecture Notes

The dashboard architecture follows these principles:

1. **Canonical Implementation**: The tracedecay dashboard is the source of truth. The Hermes wrapper is a thin reverse proxy, never a fork.

2. **Product bundle isolation**: The single-app `app-dist` bundle owns `/`.
   Legacy shell/plugin assets are isolated at `/legacy` and retained only for
   compatibility.

3. **Feature Detection**: The UI probes `/api/capabilities` to decide which features to enable, allowing graceful degradation when features are unavailable.

4. **Hermes Integration**: The compatibility wrapper remains separate from the
   single-app product frontend.

`docs/dashboard-port-handoff.md` is a historical record of the retired
multi-plugin architecture, not current implementation guidance.
