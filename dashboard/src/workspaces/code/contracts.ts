// LEGACY BOUNDARY — pending envelope migration.
// These schemas describe the pre-envelope plugin JSON endpoints
// (`/api/plugins/*`, `/api/projects`), NOT the DashboardEnvelopeV1 wire surface
// in `../../contracts/generated.ts`. They are hand-matched to their Rust
// producers and remain until these routes move to typed envelopes; new
// envelope-backed reads must use the single wire boundary in `contracts/`.
import { z } from 'zod';

/** Wire-true shapes for /api/plugins/graph (src/dashboard/graph_service.rs). */

export const KindCountSchema = z
  .object({ kind: z.string(), count: z.number() })
  .passthrough();

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
