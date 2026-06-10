# Code Graph Explorer

The third dashboard tab (`tokensave dashboard` → **Code Graph**) is an
interactive exploration surface over the project's indexed code graph
(`nodes`, `edges`, `files` in `.tokensave/tokensave.db`). It is designed for
graphs with tens of thousands of nodes: every endpoint is bounded, the canvas
only ever renders a capped, user-grown slice, and expansion is progressive.

## Backend API (`src/dashboard/graph_api.rs`)

All routes are mounted under `/api/plugins/graph/*` and read the project DB.
Under the Hermes wrapper the same routes are reverse-proxied at
`/api/plugins/hermes-intelligence/graph/*`.

| Route | Description |
|---|---|
| `GET /overview` | Landing analytics: totals, `nodes_by_kind`, `edges_by_kind`, `files_by_language` (extension-bucketed), `top_connected` (12 highest-degree symbols), `largest_files` (by `node_count`). |
| `GET /search?q=&limit=&offset=` | Paginated symbol search over name, qualified name, signature, and file path (`LIKE`, escaped). Exact-name matches rank first. Results carry full-graph `degree`. `limit` ≤ 200. |
| `GET /node/{id}` | Single node detail: signature, doc, visibility, span (`start_line`/`end_line`/columns), complexity counters, `degree`. 404 with a `detail` body when missing. |
| `GET /node/{id}/neighbors?limit=` | Depth-1 neighborhood: `callers` / `callees` (calls edges, hydrated node rows + `degree`), raw `edges` touching the node, and `edges_by_kind` counts. |
| `GET /subgraph?node_id=&limit_nodes=&limit_edges=` | One-hop subgraph for visualization. Caps default 80 nodes / 120 edges (max 250 / 500); `capped.nodes` / `capped.edges` report truncation. Accepts `q=` instead of `node_id` (best search hit becomes the seed). Nodes carry `degree` so the UI can show collapsed-neighbor counts. |
| `GET /path?from=&to=&max_depth=` | Undirected BFS shortest path between two node ids (depth default 6, max 10; visited-set capped at 20k). Returns `found`, ordered `path` ids, hydrated `nodes`, and the `edges` along the route. |

`GET /api/capabilities` advertises `features.graph: true` and lists `graph`
in `dashboards`; hosts can feature-detect via
`window.__HERMES_PLUGIN_SDK__.capabilities`.

## Frontend (`dashboard/graph/`)

TypeScript/React plugin bundle (esbuild IIFE, React externalized onto the
host SDK — same conventions as `dashboard/holographic/`). Registers as
`graph` via `window.__HERMES_PLUGINS__`, so it runs unmodified in the
standalone shell and under the Hermes wrapper. No runtime dependencies
beyond the host SDK; the force layout and charts are hand-rolled.

### Views

- **Overview (landing)** — orientation analytics as compact SVG bar charts:
  symbols by kind family, files by language, most-connected symbols, and
  largest files, plus an edge-kind strip. Chart rows are clickable: a kind
  family or language opens the canvas pre-filtered; a hub symbol focuses it.
- **Canvas (explorer)** — canvas-2D force-directed graph:
  - *Progressive exploration*: search-to-focus seeds the canvas with the
    symbol's capped subgraph; double-click a node (or Inspector buttons) to
    expand more. A `+N` badge on each node shows how many neighbors are
    still collapsed. Accumulation is soft-capped (600 nodes) with a clear
    message; **Clear** resets.
  - *Interaction*: wheel zoom around the cursor, drag-pan, node drag with
    the simulation settling around the pinned node, hover highlighting of
    the direct neighborhood (rest dims), label culling at low zoom (hubs,
    selection, and hover neighborhoods keep labels).
  - *Visual encoding*: color by kind family (functions / types / traits /
    modules / consts / impls) with a legend; node radius scales with degree;
    per-edge-kind stroke styles (calls solid amber, uses dashed blue,
    implements/extends pink, contains dotted) with directional arrowheads.
    Colors sample the shell design tokens, so light/dark themes both work.
  - *Exploration tools*: breadcrumb history of focused symbols; filter chips
    (kind family, language) plus a directory-scope prefix filter; **Find
    path** mode (pick two nodes → shortest path is fetched, merged, and
    highlighted); Inspector side panel with signature/doc/span/degree,
    one-click *Show callers* / *Show callees* (directed merges), and
    clickable caller/callee lists.

### Performance notes

- Rendering is canvas 2D with a single rAF loop that draws only while the
  simulation is hot or an interaction marked the frame dirty.
- The hand-rolled simulation (`simulation.ts`) uses link springs, a
  grid-bucketed repulsion pass, light centering, and collision separation,
  cooling via alpha decay; expansions preserve existing node positions and
  reheat instead of re-laying out.
- Labels are stroke-haloed and culled by zoom; the renderer comfortably
  sustains interaction with a few hundred visible nodes.

## Wiring

- Routes: `src/dashboard/mod.rs` (`/api/plugins/graph/*`).
- Assets: `dashboard/build.mjs` (`buildGraph()`), embedded via
  `src/dashboard/assets.rs` + `build.rs` rerun stamps.
- Hermes: `dashboard/hermes-wrapper/src/entry.js` loads `graph.js` and
  rewrites its API base; `dashboard/hermes-wrapper/plugin_api.py` proxies
  `/graph/*`.

## Tests

`tests/dashboard_graph_api_test.rs` seeds a temp project with a known
call graph (`dashboard → route_graph → render_graph`, plus a `uses` edge and
file records) and covers: capability flag, overview totals/kind/language
breakdowns, search ranking, node detail + span + doc, neighbors
(callers/callees/edge-kind grouping), subgraph node/edge caps with `capped`
flags and per-node degrees, shortest-path success and not-found, and the
`top_connected` / `largest_files` analytics.
