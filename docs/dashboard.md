# tracedecay Dashboard

The tracedecay dashboard is a local web interface for exploring your project's holographic memory, LCM (Lossless Context Management) session data, indexed code graph, and token savings / session cost accounting. It runs entirely on your machine — no external services or API keys required (the Savings & Cost tab optionally refreshes public model prices in the background; everything else is fully offline).

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
| `TRACEDECAY_GLOBAL_DB` | Pin the LCM session store to an explicit database path. When set, it wins over project-local store selection (`storage_scope` becomes `"global"`); when unset, the dashboard serves the project's `.tracedecay/sessions.db` and only falls back to `~/.tracedecay/global.db` if the project store cannot be opened |
| `TRACEDECAY_BIN` | Path to the tracedecay binary (used by Hermes wrapper for spawn mode) |
| `TRACEDECAY_DASHBOARD_PROJECT` | Project root path for Hermes dashboard spawn mode (defaults to Hermes' cwd) |
| `TRACEDECAY_DASHBOARD_URL` | Full URL to an already-running dashboard (Hermes external URL mode) |
| `HERMES_HOME` | Path to Hermes profile directory for profile-scoped plugin installation |
| `TRACEDECAY_OFFLINE` | Set to `1` to skip network requests for pricing data (Savings & Cost tab uses bundled fallback) |
| `TRACEDECAY_MODEL_PRICES_PATH` | Override the on-disk model-price cache location (default `~/.tracedecay/model-prices.json`; mainly for tests) |
| `DISABLE_TRACEDECAY` | Set to `true` to disable the MCP server entirely (exits cleanly without initializing) |

All `TRACEDECAY_*` variables (and `DISABLE_TRACEDECAY`) also accept their
legacy `TOKENSAVE_*` / `DISABLE_TOKENSAVE` spellings as fallbacks; the
`TRACEDECAY_*` name wins when both are set. Likewise, projects indexed before
the rebrand with a `.tokensave/` data directory are still honored as a
fallback wherever `.tracedecay/` paths are mentioned below.

---

## Hermes Integration

The dashboard is the canonical implementation; the Hermes plugin is a thin wrapper that reuses it.

### Installation

`tracedecay install --agent hermes` deploys the wrapper as a Hermes dashboard
plugin alongside the agent plugin, into
`<hermes_home>/plugins/tracedecay/dashboard/` (`manifest.json`,
`plugin_api.py`, and the UI bundles — all embedded in the tracedecay binary,
no source checkout needed). Hermes' dashboard plugin discovery scans
`plugins/*/dashboard/manifest.json` in both stock and forked Hermes, so a
"TraceDecay" tab (Memory / LCM / Code Graph / Savings) appears in
`hermes dashboard` after install. With `--profile <p>` the deploy lands in
`~/.hermes/profiles/<p>/plugins/tracedecay/dashboard/` and is picked up when
Hermes runs with `HERMES_HOME` pointing at that profile.

The deployed `plugin_api.py` is pinned at install time: the installing
binary's path becomes the default `TRACEDECAY_BIN`, and the profile's pinned
`project_root` (from `plugins.tracedecay.project_root` in `config.yaml`)
becomes the default `TRACEDECAY_DASHBOARD_PROJECT`. The environment variables
below still win at runtime. Reinstalls preserve the pin; pass
`--no-dashboard` to skip the dashboard deploy (and remove a previous one).

To refresh the deployed page after upgrading tracedecay without touching any
Hermes configuration, run `tracedecay update-plugin`: it rewrites the
generated plugin files and the dashboard page (re-baking the binary path and
re-reading the existing pin from `config.yaml`) for every detected install —
default profile, every `~/.hermes/profiles/*`, a `HERMES_HOME` override, and
a project-local `.hermes` in the current directory — while leaving every
`config.yaml` byte-for-byte intact. Profiles installed with `--no-dashboard`
stay dashboard-free.

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
| `TRACEDECAY_DASHBOARD_PROJECT` | No | Project root path (defaults to the install-time pinned project root, then Hermes' current working directory) |

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

Memory maintenance tools:
- **Status**: Current curation configuration and last run summary
- **Activity**: Event log of curation actions
- **Preview**: Dry-run analysis showing proposed actions (persisted to `.tracedecay/dashboard/curation_preview.json` so it survives server restarts)
- **Run Curation**: Execute deduplication (**permanently hard-DELETES** the lower-trust fact in each duplicate pair)

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
| Project-local (default) | `<project>/.tracedecay/sessions.db` | All transcript ingest for sessions belonging to that project root | `"project_local"` |
| Hermes profile | `<hermes_home>/.tracedecay/sessions.db` | Hermes-side ingest | `"hermes_profile"` |
| Global | `~/.tracedecay/global.db` | Cross-project registry (project paths, savings ledger) — **no session messages are ingested here** | `"global"` |

The dashboard serves the **project-local store** by default (where Cursor hooks and hookless-agent catch-up sweeps actually ingest). The LCM header shows a **"Project store"** or **"Global store"** badge. Every LCM API payload reports the active store via the additive `path` + `storage_scope` fields.

Setting `TRACEDECAY_GLOBAL_DB` pins the dashboard to an explicit store instead (used by tests, the smoke harness, and the Hermes wrapper, which points it at a Hermes profile's `sessions.db`). When this override is active, `storage_scope` becomes `"global"`.

#### How Ingest Works Per Tool

| Tool | Trigger |
|------|---------|
| Cursor | Cursor hooks ingest incrementally at end of turn / stop / session start (subagent transcripts included); no sweep needed |
| Claude Code, Codex, Vibe, Cline / Roo / Kilo | No hooks — discovered by a catch-up sweep that scans each tool's home transcript directory (e.g. `~/.codex/sessions`) and ingests sessions whose recorded `cwd`/project matches the served project root |
| Hermes | Hermes-side ingest into the Hermes profile store (not the project store) |

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
- Summary depth distribution
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
  Counts are cached per message — in process memory and in a
  `dashboard_token_counts` sidecar table in the global accounting DB — so
  large stores (15k+ messages) only pay the counting cost once; a background
  warm task runs at dashboard startup. Built behind the `token-counting`
  cargo feature (on by default; ~4 MB of embedded vocabularies, decoded
  lazily on first use).
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
  "memory_db": "/home/user/my-project/.tracedecay/tracedecay.db",
  "lcm_db": "/home/user/my-project/.tracedecay/sessions.db",
  "lcm_scope": "project_local",
  "features": {
    "memory": true,
    "lcm": true,
    "graph": true,
    "curation": true,
    "llm_curation": false
  },
  "dashboards": ["holographic", "hermes-lcm", "graph"]
}
```

**Fields:**
- `mode`: `"standalone"` for direct use, `"hermes"` when wrapped by Hermes
- `lcm_db` / `lcm_scope`: The LCM session store being served and its scope (`"project_local"` or `"global"`; see [Storage Scopes](#storage-scopes--where-messages-live))
- `features.memory`: Whether the project database is available
- `features.lcm`: Whether the LCM session store is available
- `features.curation`: Whether similarity-dedup curation tools are enabled
- `features.llm_curation`: Whether an LLM-backed curation planner is available. Always `false` in standalone; the Hermes wrapper flips this when it adds an LLM planner that generates ops for `POST /curate/apply`

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

#### `GET /api/plugins/holographic/curation/preview`

Last saved dry-run preview. Persisted to
`.tracedecay/dashboard/curation_preview.json`, so it survives server restarts;
applying curation (or any `/curate/apply` mutation) clears it. Staleness is
recomputed against the live fact count on every read.

**Response:**
```json
{
  "report": { /* curation plan */ },
  "saved_at": "2026-06-10T12:34:56Z",
  "stale": false,
  "stale_reason": "",
  "error": ""
}
```

#### `POST /api/plugins/holographic/curate`

Run similarity-based deduplication curation. Applying (`dry_run=false`)
**permanently deletes** the flagged facts via the canonical store delete path
(transactional row delete, FK-cascaded entity links, FTS trigger cleanup,
memory-bank dirty marking).

**Request Body:**
```json
{
  "dry_run": true  // default: true; set false to apply (DELETE) changes
}
```

**Response (dry_run=true):**
```json
{
  "ran": true,
  "dry_run": true,
  "actions": [
    {
      "op": "delete",
      "fact_id": 5,
      "duplicate_of": 3,
      "reason": "Likely duplicate of #3 (similarity 0.9623)",
      "content": "Fact content preview...",
      "similarity": 0.9623,
      "tier": "duplicate"
    }
  ],
  "hygiene_candidates": {
    "secret_like": [ /* review_required candidates, tier "secret_like" */ ],
    "transient": [ /* review_required candidates, tier "transient" */ ],
    "supersession": [
      {
        "recommended_op": "delete",
        "fact_id": 4,
        "superseded_by": 7,
        "similarity": 0.8123,
        "reason": "Possible supersession: negation/state-change cue ...",
        "content": "Fact content preview...",
        "status": "candidate",
        "review_required": true,
        "access_count": 2,
        "tier": "supersession"
      }
    ]
  },
  "counts": { "delete": 1 },
  "coverage": {
    "scanned": 500,
    "active_total": 500,
    "due_remaining": 0
  },
  "provider": "tracedecay",
  "mode": "similarity_dedup"
}
```

`hygiene_candidates` is the deterministic rule-based evidence set
(secret-like content, transient run output, negation-cue supersession pairs).
These entries are **never auto-applied** — `dry_run=false` only executes
`actions` (dedup deletes); a reviewer (human, the `tracedecay memory curate
--llm` two-phase flow, or the Hermes LLM wrapper) confirms hygiene candidates by
submitting explicit delete/merge ops through `POST /curate/apply`. Low trust by
itself is not a delete signal; trust only helps calibrate candidate confidence.
The dedup planner also applies access-count delete-reluctance: the
higher-access fact of a pair is never auto-proposed as the loser unless the
similarity is extreme (≥ 0.98). Helpful feedback raises trust, and recall access
updates `access_count`/`last_recalled_at`, giving useful facts protection during
curation review.

**Response (dry_run=false):**
Same structure with `applied_counts` showing what was actually deleted and
`skipped_actions` counting per-action failures (e.g. already-deleted ids).

#### `POST /api/plugins/holographic/curate/apply`

Generic curation-ops apply endpoint. This is the contract external planners
(e.g. an LLM-backed Hermes wrapper, advertised via `features.llm_curation`)
build against. Per-op failures are reported per-op in `results`; the request
only fails wholesale (400) on a malformed body.

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
  "path": "/home/user/my-project/.tracedecay/sessions.db",
  "storage_scope": "project_local",
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
  "path": "/home/user/my-project/.tracedecay/sessions.db",
  "storage_scope": "project_local",
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
  "path": "/home/user/my-project/.tracedecay/sessions.db",
  "storage_scope": "project_local",
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
| `features.curation` | Similarity-dedup curation tools are available | Show Curation panel, enable curate actions |
| `features.llm_curation` | An LLM-backed curation planner is available (Hermes wrapper only) | Enable LLM plan actions that target `POST /curate/apply` |

There is no archive flag: curation deletes are permanent, and no archive or
restore endpoints exist. Always check the capability flags rather than
assuming availability — they may change based on database state and host
(standalone vs Hermes).

---

## Frontend Development

The dashboard frontend source lives in `dashboard/`:

| Directory | Contents |
|-----------|----------|
| `dashboard/shell/` | Standalone host shell (React 19, Hermes-compatible SDK) |
| `dashboard/holographic/` | Holographic memory plugin bundle |
| `dashboard/lcm/` | LCM plugin bundle |
| `dashboard/graph/` | Code Graph explorer plugin bundle |
| `dashboard/hermes-wrapper/` | Hermes-side thin wrapper |

### Building

```bash
cd dashboard
npm install
npm run build
```

This command:
1. Builds the shell bundle (React 19 + esbuild)
2. Rebuilds the holographic bundle from source
3. Builds the LCM bundle
4. Builds the Code Graph bundle
5. Assembles the hermes-wrapper dist (including `graph.js`)

### Smoke Testing

Playwright-based smoke tests verify tab rendering, search interaction, and viewport responsiveness. Unless `--url=` points at an already-running server, the smoke script is hermetic: it creates a throwaway temp project, runs `tracedecay init` on it, and serves the dashboard from there — so it works on fresh checkouts (and CI) with no pre-existing `.tracedecay/` index:

```bash
# Empty-state LCM (default global.db has no LCM data)
TRACEDECAY_GLOBAL_DB=/tmp/tracedecay-dashboard-lcm-empty.db npm run smoke -- --expect-lcm=empty

# Non-empty LCM (requires seeded database)
TRACEDECAY_GLOBAL_DB=/tmp/tracedecay-dashboard-lcm-nonempty.db npm run smoke -- --expect-lcm=non-empty
```

### Asset Embedding

Static assets are embedded at compile time via `include_bytes!` in `src/dashboard/assets.rs`. After building the frontend, you must rebuild the Rust binary to pick up new assets:

```bash
cd dashboard && npm run build
cd .. && cargo build --bin tracedecay
```

The `build.rs` script emits `cargo::rerun-if-changed` directives for all embedded assets, so the binary automatically rebuilds when dist files change.

When the dist files are missing entirely (fresh checkout, `cargo install
--path .`), `build.rs` builds them automatically: it runs `npm ci` (falling
back to `npm install`) and `npm run build` in `dashboard/` and embeds the
result. If npm is not on PATH, the build fails fast with instructions instead.

### Packaging / crates.io

`Cargo.toml` uses an explicit `package.include` whitelist that ships the
**prebuilt** `dashboard/*/dist` bundles inside the crate package. This means:

- `cargo package` / `cargo publish` must be run after `cd dashboard && npm ci
  && npm run build` (the release workflow does this); the package verify step
  then compiles without touching npm.
- Crates.io consumers (`cargo install tracedecay`) and docs.rs need **no**
  Node.js toolchain — the embedded assets come straight from the package.

### Development Workflow

For rapid frontend iteration without rebuilding Rust:

```bash
# 1. Build once to generate dist/
cd dashboard && npm run build

# 2. Serve with a tool that supports live reload for static files
# (The dashboard server serves embedded bytes, not filesystem files)

# 3. After frontend changes, rebuild and restart the server
npm run build
cargo run -- dashboard
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

# Check the project session store for rows
ls -la .tracedecay/sessions.db
sqlite3 .tracedecay/sessions.db 'SELECT COUNT(*) FROM lcm_raw_messages'

# The LCM header shows which store is being served ("Project store" /
# "Global store") and its path. If it shows the global DB unexpectedly,
# check whether TRACEDECAY_GLOBAL_DB is set — it pins the store:
echo "$TRACEDECAY_GLOBAL_DB"

# Pin to an explicit store if needed
export TRACEDECAY_GLOBAL_DB=/path/to/sessions.db
tracedecay dashboard
```

### Frontend Assets Not Updating

```bash
# After editing dashboard/ source files, changes don't appear

# The dashboard serves assets embedded at compile time.
# You must rebuild both frontend and Rust:

cd dashboard && npm run build
cd .. && cargo build --bin tracedecay

# Or touch the assets file to force re-embedding:
touch src/dashboard/assets.rs && cargo build
```

### Build Errors: Dashboard Assets Missing

```bash
# Error: missing dashboard dist assets ... npm was not found on PATH

# build.rs builds missing assets automatically when npm is available.
# This error means npm is not installed; install Node.js 22+, or build
# the frontend manually before the Rust binary:
cd dashboard && npm install && npm run build
cd .. && cargo build --bin tracedecay
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

2. **UI Bundle Portability**: Both the standalone shell and the Hermes wrapper provide a compatible SDK so the same plugin bundles work in both hosts.

3. **Feature Detection**: The UI probes `/api/capabilities` to decide which features to enable, allowing graceful degradation when features are unavailable.

4. **Hermes Integration**: The wrapper uses a `new Function()` + Proxy evaluation strategy so child bundles don't pollute the global scope for concurrent Hermes plugins.

For full architectural details, see `docs/dashboard-port-handoff.md` (internal documentation).
