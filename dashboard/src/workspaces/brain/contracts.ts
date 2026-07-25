// LEGACY BOUNDARY — pending envelope migration.
// These schemas describe the pre-envelope plugin JSON endpoints
// (`/api/plugins/*`, `/api/projects`), NOT the DashboardEnvelopeV1 wire surface
// in `../../contracts/generated.ts`. They are hand-matched to their Rust
// producers and remain until these routes move to typed envelopes; new
// envelope-backed reads must use the single wire boundary in `contracts/`.
import { z } from 'zod';

/** Wire-true shapes for GET /api/projects (src/dashboard/projects.rs `list`). */

export const ProjectRegistryEntrySchema = z
  .object({
    project_id: z.string(),
    label: z.string(),
    project_root: z.string(),
    canonical_root: z.string(),
    kind: z.string(),
    default_branch: z.string().nullable().optional(),
    branches: z.array(z.string()),
    store_count: z.number(),
    graph_scope_count: z.number(),
    artifact_count: z.number(),
    alias_count: z.number(),
    last_seen_at: z.number(),
    is_active: z.boolean().optional(),
    /** Graph mass (total nodes/edges across the project's stores). Optional:
     * the registry does not serve it yet; neurons size by store_count until
     * the backend lands mass telemetry, then upgrade automatically. */
    graph_node_count: z.number().optional(),
    graph_edge_count: z.number().optional(),
  })
  .passthrough();
export type ProjectRegistryEntry = z.infer<typeof ProjectRegistryEntrySchema>;

export const ProjectRepoGroupSchema = z
  .object({
    label: z.string(),
    git_common_dir: z.string().nullable().optional(),
    project_count: z.number(),
    branches: z.array(z.string()),
    projects: z.array(ProjectRegistryEntrySchema),
  })
  .passthrough();
export type ProjectRepoGroup = z.infer<typeof ProjectRepoGroupSchema>;

export const ProjectsPayloadSchema = z
  .object({
    status: z.enum(['ok', 'missing_registry', 'registry_unavailable']),
    truncated: z.boolean().optional(),
    active_project_id: z.string().nullable().optional(),
    active_project_root: z.string().optional(),
    summary: z
      .object({
        project_count: z.number(),
        repo_count: z.number(),
        truncated: z.boolean(),
      })
      .passthrough(),
    project_tree: z.array(ProjectRepoGroupSchema),
  })
  .passthrough();
export type ProjectsPayload = z.infer<typeof ProjectsPayloadSchema>;

/* ------------------------------------------------------------------------ *
 * Scoped Brain. When a project is selected the Brain becomes THAT project's
 * brain, and these are the surfaces it is composed from. Two tiers, because
 * the daemon genuinely has two:
 *
 *  - `GET /api/projects/{id}` (src/dashboard/projects.rs `context`) answers for
 *    every REGISTERED project — its stores, the graph scopes (branches) inside
 *    them, the artifacts on disk with their sizes, and every alias path the
 *    project has been seen at. This always resolves, so it is the backbone.
 *
 *  - The project-scoped gateway (`/api/projects/{id}/…` → `/api/…` against that
 *    project's state) answers only while that project's graph is MOUNTED, which
 *    in practice means the active one; every other project returns 404
 *    "registered project graph is not mounted". So the code-graph field, the
 *    memory bank and the session analytics are composed when they are there and
 *    the registry backbone carries the surface when they are not. Nothing is
 *    substituted for a missing read.
 * ------------------------------------------------------------------------ */

export const ProjectStoreArtifactSchema = z
  .object({
    artifact_kind: z.string(),
    relpath: z.string(),
    size_bytes: z.number().nullable().optional(),
    updated_at: z.number().nullable().optional(),
  })
  .passthrough();

export const ProjectGraphScopeSchema = z
  .object({
    graph_scope_id: z.string(),
    branch_name: z.string(),
    db_relpath: z.string().optional(),
    last_synced_at: z.number().nullable().optional(),
    writable: z.boolean().optional(),
  })
  .passthrough();

export const ProjectStoreSchema = z
  .object({
    store: z
      .object({
        store_id: z.string(),
        store_kind: z.string().optional(),
        storage_mode: z.string().optional(),
        store_relpath: z.string().optional(),
        last_write_at: z.number().nullable().optional(),
        last_verified_at: z.number().nullable().optional(),
      })
      .passthrough(),
    graph_scopes: z.array(ProjectGraphScopeSchema).optional(),
    artifacts: z.array(ProjectStoreArtifactSchema).optional(),
  })
  .passthrough();
export type ProjectStore = z.infer<typeof ProjectStoreSchema>;

export const ProjectAliasSchema = z
  .object({ alias_path: z.string(), last_seen_at: z.number() })
  .passthrough();
export type ProjectAlias = z.infer<typeof ProjectAliasSchema>;

/** GET /api/projects/{project_id} (src/dashboard/projects.rs `context`). */
export const ProjectContextPayloadSchema = z
  .object({
    status: z.string(),
    is_active: z.boolean().optional(),
    project: ProjectRegistryEntrySchema.nullable().optional(),
    aliases: z.array(ProjectAliasSchema).optional(),
    stores: z.array(ProjectStoreSchema).optional(),
  })
  .passthrough();
export type ProjectContextPayload = z.infer<typeof ProjectContextPayloadSchema>;

/** GET /api/plugins/graph/subgraph (src/dashboard/graph_api.rs `subgraph`),
 * read through the scoped gateway. The unseeded call returns the project's
 * most-connected neighborhood, already capped by the daemon. */
export const ScopedSubgraphNodeSchema = z
  .object({
    id: z.string(),
    kind: z.string(),
    name: z.string().nullable().optional(),
    qualified_name: z.string().nullable().optional(),
    file_path: z.string().nullable().optional(),
    degree: z.number().nullable().optional(),
  })
  .passthrough();
export type ScopedSubgraphNode = z.infer<typeof ScopedSubgraphNodeSchema>;

export const ScopedSubgraphPayloadSchema = z
  .object({
    nodes: z.array(ScopedSubgraphNodeSchema).optional(),
    edges: z
      .array(
        z
          .object({ source: z.string(), target: z.string(), kind: z.string().optional() })
          .passthrough(),
      )
      .optional(),
    capped: z
      .object({ nodes: z.boolean().optional(), edges: z.boolean().optional() })
      .passthrough()
      .optional(),
  })
  .passthrough();
export type ScopedSubgraphPayload = z.infer<typeof ScopedSubgraphPayloadSchema>;

/** GET /api/plugins/graph/overview (src/dashboard/graph_api.rs `overview`). */
export const ScopedGraphOverviewSchema = z
  .object({
    totals: z
      .object({ nodes: z.number(), edges: z.number(), files: z.number() })
      .passthrough(),
    nodes_by_kind: z
      .array(z.object({ kind: z.string(), count: z.number() }).passthrough())
      .optional(),
    files_by_language: z
      .array(z.object({ language: z.string(), count: z.number() }).passthrough())
      .optional(),
  })
  .passthrough();
export type ScopedGraphOverview = z.infer<typeof ScopedGraphOverviewSchema>;

/** GET /api/plugins/holographic/status (src/dashboard/memory_api.rs `status`). */
export const ScopedMemoryStatusSchema = z
  .object({
    exists: z.boolean().optional(),
    memory: z
      .object({
        fact_count: z.number().optional(),
        entity_count: z.number().optional(),
        bank_count: z.number().optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();
export type ScopedMemoryStatus = z.infer<typeof ScopedMemoryStatusSchema>;

/** GET /api/plugins/analytics/overview (src/dashboard/analytics_api.rs). */
export const ScopedAnalyticsOverviewSchema = z
  .object({
    available: z.boolean().optional(),
    usage: z
      .object({
        available: z.boolean().optional(),
        event_count: z.number().optional(),
        message_count: z.number().optional(),
        by_category: z
          .array(
            z
              .object({ category: z.string(), events: z.number(), kind: z.string().optional() })
              .passthrough(),
          )
          .optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();
export type ScopedAnalyticsOverview = z.infer<typeof ScopedAnalyticsOverviewSchema>;
