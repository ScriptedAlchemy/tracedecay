# Dashboard

The embedded dashboard is the daemon’s graphical client for code intelligence,
project memory, lossless context, work, configuration, diagnostics, and usage.
Start it with:

```bash
tracedecay dashboard --open
```

The dashboard uses generated Rust API contracts and the same application
operations as CLI, MCP, LSP, SDK, hooks, and host integrations. Browser code
never opens TraceDecay or host storage.

Every view displays the selected project/profile scope, evidence authority,
repository and generation provenance, coverage, and receipts. Warming, partial,
unavailable, denied, cancelled, retained, and recreation-required outcomes are
distinct UI states, never a blank successful panel.

Structure views present Grafeo-backed graph and vector results supplied by the
daemon. Conversation and memory views hydrate authorized content from its owning
authority. Work controls separate proposal, authorization, execution, and
repository effects. Diagnostics are read-only; maintenance actions are explicit
daemon operations with authorization and receipts.

The dashboard binds to loopback and is intended for the local operator. It
supports keyboard navigation, screen readers, responsive layouts, reduced
motion, bounded graph rendering, and a non-graph form for selected evidence.

## Code graph plugin API

The legacy `/legacy` frontend's Code Graph tab reads a project-local, bounded
API over the indexed code graph (`nodes`, `edges`, `files` in
`.tracedecay/tracedecay.db`), mounted in
`crates/tracedecay-dashboard-api/src/lib.rs` (see `graph_api`). Under the
Hermes wrapper the same routes are reverse-proxied at
`/api/plugins/tracedecay/graph/*`.

| Route | Description |
|---|---|
| `GET /overview` | Landing analytics: totals, `nodes_by_kind`, `edges_by_kind`, `files_by_language` (extension-bucketed), `top_connected` (12 highest-degree symbols), `largest_files` (by `node_count`). |
| `GET /search?q=&limit=&offset=` | Paginated symbol search over name, qualified name, signature, and file path (`LIKE`, escaped). Exact-name matches rank first. Results carry full-graph `degree`. `limit` ≤ 200. |
| `GET /node/{id}` | Single node detail: signature, doc, visibility, span (`start_line`/`end_line`/columns), complexity counters, `degree`. 404 with a `detail` body when missing. |
| `GET /node/{id}/neighbors?limit=` | Depth-1 neighborhood: `callers` / `callees` (calls edges, hydrated node rows + `degree`), raw `edges` touching the node, and `edges_by_kind` counts. |
| `GET /subgraph?node_id=&limit_nodes=&limit_edges=` | One-hop subgraph for visualization. Caps default 80 nodes / 120 edges (max 250 / 500); `capped.nodes` / `capped.edges` report truncation. Accepts `q=` instead of `node_id` (best search hit becomes the seed; a query with no hit returns an empty payload). With no seed at all it returns the **default overview slice** (`mode: "default"`): the top-degree hubs plus the edges among them — hubs with no edges to other hubs are pruned in favor of interconnected ones, and isolated nodes only fill leftover capacity (so tiny or edge-free indexes still render). Seeded responses carry `mode: "seeded"`. Nodes carry `degree` so the UI can show collapsed-neighbor counts. |
| `GET /path?from=&to=&max_depth=` | Undirected BFS shortest path between two node ids (depth default 6, max 10; visited-set capped at 20k). Returns `found`, ordered `path` ids, hydrated `nodes`, and the `edges` along the route. |

`GET /api/capabilities` advertises `features.graph: true` and lists `graph`
in `dashboards`; hosts can feature-detect via
`window.__HERMES_PLUGIN_SDK__.capabilities`. Full legacy-frontend history
(views, wiring, tests) lives in git history under `docs/archive/graph-explorer.md`.

## Hermes plugin and LCM diagnostics

Install Hermes through TraceDecay so the generated user plugin and its wrapper
configuration stay together:

```bash
tracedecay install --agent hermes
tracedecay doctor
```

The installer writes the plugin under `~/.hermes/plugins/tracedecay/` and
enables it through `plugins.enabled` in `~/.hermes/config.yaml`. Plugin-owned
settings live in the `plugins.tracedecay` configuration block. Hermes' own
`compression.enabled` setting is the global automatic-compaction switch; other
compression settings remain host configuration rather than TraceDecay storage
identity.

The wrapper invokes the current TraceDecay tool schema through
`tracedecay tool <name> --json --args <json>` and uses the host's real project
root or working directory for routing. The native context engine exposes
`lcm_grep`, `lcm_load_session`, `lcm_describe`, `lcm_expand`,
`lcm_expand_query`, `lcm_status`, and `lcm_doctor`; each maps to the matching
daemon-routed LCM operation.

The wrapper reads these runtime environment variables before its installed
defaults:

| Variable | Effect |
|---|---|
| `TRACEDECAY_DASHBOARD_URL` | Uses the specified existing dashboard server instead of spawning one. |
| `TRACEDECAY_BIN` | Selects the `tracedecay` executable used when the wrapper starts the dashboard. |
| `TRACEDECAY_DASHBOARD_PROJECT` | Selects the project root passed to the dashboard; when unset, the wrapper uses the Hermes process working directory. |

Hermes homes and profiles never select a TraceDecay project or store. When the
wrapper starts the dashboard itself, that server is loopback-bound and inherits
Hermes dashboard-session protection. `TRACEDECAY_DASHBOARD_URL` instead accepts
an arbitrary HTTP(S) endpoint; setting it changes the network and trust
boundary, so the operator must secure that endpoint and its transport. Use the
variables only for the wrapper's runtime route, not as a store selector.

For a Hermes problem, run the doctor command first. For a session or
compression problem, inspect `lcm_status` and `lcm_doctor` (or the matching
`tracedecay_lcm_*` operation) and retain the reported coverage, retention,
payload, and provenance state. `unavailable`, `partial`, `denied`, and
`refresh_required` are diagnostic outcomes, not instructions to inspect a
database or rebuild it by hand. Authorized maintenance remains a separate
daemon operation with its own preview and receipt.
