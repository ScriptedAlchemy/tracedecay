/**
 * UNCONTRACTED — the legacy graph plugin routes.
 *
 *   GET /api/plugins/graph/overview
 *   GET /api/plugins/graph/search
 *   GET /api/plugins/graph/subgraph
 *   GET /api/plugins/graph/node/{id}/neighbors
 *
 * Producer: `src/dashboard/graph_service.rs` (`overview_payload`,
 * `search_payload`, `subgraph_payload`, `neighbors_payload`), all returning
 * `serde_json::Value`. Nothing here is generated; see `./README.md`.
 *
 * These are read twice over: the Code workspace calls them directly, and the
 * Brain workspace calls the same handlers through the project-scoped gateway
 * (`/api/projects/{id}/plugins/graph/...`, which `src/dashboard/mod.rs` rewrites
 * to `/api/...` against that project's state). Same handler, same body — but
 * they used to be modelled twice, as `GraphNode`/`SubgraphPayload`/
 * `GraphOverviewPayload` in `workspaces/code/contracts.ts` and as
 * `ScopedSubgraphNode`/`ScopedSubgraphPayload`/`ScopedGraphOverview` in
 * `workspaces/brain/contracts.ts`. The `Scoped*` copies had gone all-optional
 * against a producer that emits every field on every response, so the two
 * described different contracts for one route and nothing connected them.
 */
import { z } from 'zod';

export const KindCountSchema = z.object({ kind: z.string(), count: z.number() }).passthrough();

/** `overview_payload`. Every key is emitted on success; the optional markers
 * are retained from the Code workspace's original reading rather than tightened
 * here, because this route has no contract to tighten against. */
export const GraphOverviewPayloadSchema = z
  .object({
    totals: z
      .object({
        nodes: z.number(),
        edges: z.number(),
        files: z.number(),
      })
      .passthrough(),
    nodes_by_kind: z.array(KindCountSchema).optional(),
    edges_by_kind: z.array(KindCountSchema).optional(),
    files_by_language: z.array(z.record(z.unknown())).optional(),
    top_connected: z.array(z.record(z.unknown())).optional(),
    largest_files: z.array(z.record(z.unknown())).optional(),
  })
  .passthrough();
export type GraphOverviewPayload = z.infer<typeof GraphOverviewPayloadSchema>;

export const GraphNodeSchema = z
  .object({
    id: z.string(),
    kind: z.string(),
    name: z.string().nullable().optional(),
    qualified_name: z.string().nullable().optional(),
    file_path: z.string().nullable().optional(),
    start_line: z.number().nullable().optional(),
    end_line: z.number().nullable().optional(),
    signature: z.string().nullable().optional(),
    visibility: z.string().nullable().optional(),
    degree: z.number().optional(),
  })
  .passthrough();
export type GraphNode = z.infer<typeof GraphNodeSchema>;

export const GraphSearchPayloadSchema = z
  .object({
    total: z.number().optional(),
    results: z.array(GraphNodeSchema).optional(),
  })
  .passthrough();
export type GraphSearchPayload = z.infer<typeof GraphSearchPayloadSchema>;

export const SubgraphEdgeSchema = z
  .object({
    source: z.string(),
    target: z.string(),
    kind: z.string().optional(),
  })
  .passthrough();

/**
 * `neighbors_payload`. Feeds the TRACE drill-in.
 *
 * `callers` / `callees` are `calls` edges only and carry ONE ROW PER EDGE — a
 * caller with four call sites appears four times with different `edge_line`,
 * which is the only place the wire carries a call-site count. `edges` is every
 * edge kind incident on the requested node (`source = ?1 OR target = ?1`), so a
 * `contains` row here is always the container OF that node. Both lists are
 * truncated at `limit`.
 */
export const NeighborRowSchema = GraphNodeSchema.extend({
  edge_kind: z.string().nullable().optional(),
  edge_line: z.number().nullable().optional(),
});

export const NeighborEdgeSchema = z
  .object({
    source: z.string(),
    target: z.string(),
    kind: z.string(),
    line: z.number().nullable().optional(),
    source_name: z.string().nullable().optional(),
    target_name: z.string().nullable().optional(),
  })
  .passthrough();

export const GraphNeighborsPayloadSchema = z
  .object({
    node_id: z.string(),
    depth: z.number().optional(),
    limit: z.number().optional(),
    callers: z.array(NeighborRowSchema).optional(),
    callees: z.array(NeighborRowSchema).optional(),
    edges: z.array(NeighborEdgeSchema).optional(),
    edges_by_kind: z.array(KindCountSchema).optional(),
  })
  .passthrough();
export type GraphNeighborsPayload = z.infer<typeof GraphNeighborsPayloadSchema>;

/** `subgraph_payload`. Every branch of the handler — default slice, seeded
 * slice, and the explicit no-match case — writes all five keys, so these are
 * required. The unseeded call returns the project's most-connected
 * neighborhood, already capped by the daemon. */
export const SubgraphPayloadSchema = z
  .object({
    seed_id: z.string().nullable(),
    mode: z.string(),
    nodes: z.array(GraphNodeSchema),
    edges: z.array(SubgraphEdgeSchema),
    capped: z.object({ nodes: z.boolean(), edges: z.boolean() }).passthrough(),
  })
  .passthrough();
export type SubgraphPayload = z.infer<typeof SubgraphPayloadSchema>;
