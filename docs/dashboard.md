# tokensave Dashboard

The tokensave dashboard is a local web interface for exploring your project's holographic memory, LCM (Lossless Context Management) session data, and indexed code graph. It runs entirely on your machine — no external services, API keys, or network connections required.

---

## Table of Contents

- [Quick Start](#quick-start)
- [Standalone Usage](#standalone-usage)
- [Hermes Integration](#hermes-integration)
- [Dashboard Tabs](#dashboard-tabs)
  - [Holographic Memory](#holographic-memory)
  - [LCM](#lcm)
  - [Code Graph](#code-graph)
- [API Reference](#api-reference)
  - [Capability Discovery](#capability-discovery)
  - [Holographic Memory API](#holographic-memory-api)
  - [LCM API](#lcm-api)
- [Capability Flags](#capability-flags)
- [Frontend Development](#frontend-development)
- [Troubleshooting](#troubleshooting)

---

## Quick Start

```bash
# Start the dashboard on the default port (7341)
tokensave dashboard

# Output:
# tokensave dashboard listening on http://127.0.0.1:7341/
# Serving project /home/user/my-project
# Press Ctrl+C to stop.

# Then open http://127.0.0.1:7341/ in your browser
```

---

## Standalone Usage

### Command-Line Flags

```bash
tokensave dashboard [OPTIONS]

Options:
  -p, --path <PATH>  Project path (default: current directory, with discovery)
      --host <HOST>  Address to bind [default: 127.0.0.1]
      --port <PORT>  Port to listen on (0 = pick a free port) [default: 7341]
      --open         Open the dashboard URL in the default browser after the server starts
  -h, --help         Print help
```

### MCP Tool

MCP-connected agents can manage the dashboard without a terminal via the
`tokensave_dashboard` tool. It starts the server for the current project as a
background task inside the MCP server and returns the listening URL.
Idempotent: if a dashboard is already running, the existing URL is returned.
Pass `action: "stop"` to shut it down; optional `host`/`port` arguments match
the CLI defaults.

### Port 0 (Auto-Select)

When `--port 0` is specified, the OS assigns a free port. The server prints a parseable URL on stdout as the first line:

```bash
tokensave dashboard --port 0
# tokensave dashboard listening on http://127.0.0.1:45678/
```

This format is stable and used by wrapper tools (like the Hermes plugin) to discover the server URL.

### Environment Variables

| Variable | Description |
|----------|-------------|
| `TOKENSAVE_GLOBAL_DB` | Override the path to the global database used for LCM data (default: `~/.tokensave/global.db`) |

---

## Hermes Integration

The dashboard is the canonical implementation; the Hermes plugin is a thin wrapper that reuses it. Two modes are supported:

### 1. Spawn Mode (Default)

Hermes automatically launches the dashboard server and proxies requests to it. The server is started with `--port 0` and the URL is parsed from stdout.

**Environment variables used:**

| Variable | Required | Description |
|----------|----------|-------------|
| `TOKENSAVE_BIN` | Yes | Path to the tokensave binary |
| `TOKENSAVE_DASHBOARD_PROJECT` | No | Project root path (defaults to Hermes' current working directory) |

**Example:**
```bash
export TOKENSAVE_BIN=/usr/local/bin/tokensave
export TOKENSAVE_DASHBOARD_PROJECT=/home/user/my-project
hermes dashboard
```

### 2. External URL Mode

Point Hermes at an already-running dashboard instance.

**Environment variable:**

| Variable | Required | Description |
|----------|----------|-------------|
| `TOKENSAVE_DASHBOARD_URL` | Yes | Full URL to a running tokensave dashboard (e.g., `http://127.0.0.1:7341/`) |

**Example:**
```bash
# Terminal 1: Start dashboard
tokensave dashboard --port 7341

# Terminal 2: Tell Hermes to use it
export TOKENSAVE_DASHBOARD_URL=http://127.0.0.1:7341/
hermes dashboard
```

When using external URL mode, the Hermes plugin acts as a reverse proxy, rewriting request paths from `/api/plugins/hermes-intelligence/*` to the tokensave dashboard's native paths.

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
- **Preview**: Dry-run analysis showing proposed actions
- **Run Curation**: Execute deduplication (permanently DELETES the lower-trust fact in each duplicate pair)

Curation is implemented as similarity-based deduplication (no LLM calls). It proposes hard-deleting the lower-trust fact in each `likely_duplicate` pair. **Deletion is permanent — there is no archive or restore.** Deleted facts are removed from `memory_facts` along with their entity links (FK cascade) and FTS rows (trigger), so they immediately disappear from `tokensave_fact_store` recall.

External planners (such as an LLM-backed Hermes wrapper, gated behind the `features.llm_curation` flag) can apply their own delete/merge operations through `POST /curate/apply` (see API reference).

### LCM

The LCM (Lossless Context Management) tab visualizes session data from the global database.

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
graph (`nodes`, `edges`, `files` in `.tokensave/tokensave.db`).

- **Overview**: orientation analytics — symbols by kind family, files by
  language, most-connected symbols, largest files, and an edge-kind strip.
  Chart rows are clickable and open the canvas pre-filtered or focused.
- **Canvas**: a force-directed canvas-2D explorer with search-to-focus,
  progressive neighbor expansion (double-click or Inspector buttons), kind /
  language / directory-scope filters, callers/callees drilldown, and a
  **Find path** mode that highlights the shortest path between two symbols.

The backend routes live under `/api/plugins/graph/*` (proxied by the Hermes
wrapper at `/api/plugins/hermes-intelligence/graph/*`). See
[graph-explorer.md](graph-explorer.md) for the full API table, frontend
design, and performance notes.

---

## API Reference

All API endpoints return JSON. The dashboard mirrors the original Hermes plugin API paths for compatibility.

### Capability Discovery

#### `GET /api/capabilities`

Returns feature flags and server configuration. Used by the UI and wrappers to determine which panels/actions to enable.

**Response:**
```json
{
  "name": "tokensave-dashboard",
  "version": "6.1.3",
  "mode": "standalone",
  "project_root": "/home/user/my-project",
  "memory_db": "/home/user/my-project/.tokensave/tokensave.db",
  "lcm_db": "/home/user/.tokensave/global.db",
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
- `features.memory`: Whether the project database is available
- `features.lcm`: Whether the global database is available
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
    "path": "/path/to/tokensave.db",
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

Last saved dry-run preview (if any exists from current server session).

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
  "counts": { "delete": 1 },
  "coverage": {
    "scanned": 500,
    "active_total": 500,
    "due_remaining": 0
  },
  "provider": "tokensave",
  "mode": "similarity_dedup"
}
```

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
  "path": "/home/user/.tokensave/global.db",
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
  "path": "/home/user/.tokensave/global.db",
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
  "path": "/home/user/.tokensave/global.db",
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
| `features.lcm` | Global database is accessible | Show LCM tab |
| `features.graph` | Code-graph API is available | Show Code Graph tab |
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

Playwright-based smoke tests verify tab rendering, search interaction, and viewport responsiveness:

```bash
# Empty-state LCM (default global.db has no LCM data)
TOKENSAVE_GLOBAL_DB=/tmp/tokensave-dashboard-lcm-empty.db npm run smoke -- --expect-lcm=empty

# Non-empty LCM (requires seeded database)
TOKENSAVE_GLOBAL_DB=/tmp/tokensave-dashboard-lcm-nonempty.db npm run smoke -- --expect-lcm=non-empty
```

### Asset Embedding

Static assets are embedded at compile time via `include_bytes!` in `src/dashboard/assets.rs`. After building the frontend, you must rebuild the Rust binary to pick up new assets:

```bash
cd dashboard && npm run build
cd .. && cargo build --bin tokensave
```

The `build.rs` script emits `cargo::rerun-if-changed` directives for all embedded assets, so the binary automatically rebuilds when dist files change.

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
tokensave dashboard --port 8080

# Option 2: Let the OS pick a free port
tokensave dashboard --port 0

# Option 3: Find and stop the existing process
lsof -i :7341
kill <PID>
```

### Missing Project Database

```bash
# Dashboard starts but Holographic Memory tab shows empty/error

# Ensure you've initialized tokensave in your project
cd /path/to/project
tokensave init
tokensave sync

# Then restart the dashboard
tokensave dashboard
```

### Missing LCM Data

```bash
# LCM tab shows empty state

# This is expected if you haven't used LCM-enabled tools yet.
# The global database is populated by:
# - Cursor transcript ingestion (via hooks)
# - Explicit LCM tool calls

# Check if global database exists
ls -la ~/.tokensave/global.db

# Set custom path if needed
export TOKENSAVE_GLOBAL_DB=/path/to/global.db
tokensave dashboard
```

### Frontend Assets Not Updating

```bash
# After editing dashboard/ source files, changes don't appear

# The dashboard serves assets embedded at compile time.
# You must rebuild both frontend and Rust:

cd dashboard && npm run build
cd .. && cargo build --bin tokensave

# Or touch the assets file to force re-embedding:
touch src/dashboard/assets.rs && cargo build
```

### Build Errors: Dashboard Assets Missing

```bash
# Error: couldn't read dashboard/shell/dist/shell.js

# The frontend must be built before the Rust binary:
cd dashboard && npm install && npm run build
cd .. && cargo build --bin tokensave
```

### Hermes Wrapper Connection Failed

```bash
# Hermes shows "Connection refused" or timeout

# Check that TOKENSAVE_BIN is correct
export TOKENSAVE_BIN=$(which tokensave)

# For external URL mode, verify the server is running
curl http://127.0.0.1:7341/api/capabilities

# Check the Hermes plugin logs for spawn errors
```

### Slow Initial Load

The dashboard may be slow on first load if:
- The project database is very large (millions of nodes)
- The global database is on a network filesystem

Mitigations:
- Run `tokensave sync` before starting the dashboard
- Ensure `~/.tokensave/global.db` is on local storage
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

1. **Canonical Implementation**: The tokensave dashboard is the source of truth. The Hermes wrapper is a thin reverse proxy, never a fork.

2. **UI Bundle Portability**: Both the standalone shell and the Hermes wrapper provide a compatible SDK so the same plugin bundles work in both hosts.

3. **Feature Detection**: The UI probes `/api/capabilities` to decide which features to enable, allowing graceful degradation when features are unavailable.

4. **Hermes Integration**: The wrapper uses a `new Function()` + Proxy evaluation strategy so child bundles don't pollute the global scope for concurrent Hermes plugins.

For full architectural details, see `docs/dashboard-port-handoff.md` (internal documentation).
