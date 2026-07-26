/**
 * UNCONTRACTED — `GET /api/projects` and `GET /api/projects/{project_id}`.
 *
 * Producer: `src/dashboard/projects.rs` (`list`, `context`), serializing the
 * types in `src/project_registry.rs`. Neither handler has a `JsonSchema` DTO,
 * so nothing here is generated; see `./README.md`.
 *
 * This module replaces two hand-written copies of the same route — one in
 * `workspaces/brain/contracts.ts` and one in `workspaces/delivery/contracts.ts`
 * — which had drifted apart under different export names
 * (`ProjectsPayloadSchema` vs `DeliveryProjectsPayloadSchema`) while modelling
 * the identical body.
 *
 * Both copies also got the degraded responses wrong, in the direction that
 * throws in front of a user. `list` answers `missing_registry` and
 * `registry_unavailable` with an explicit `"summary": null`,
 * `"project_tree": null`, `"projects": null`, `"truncated": null`, and neither
 * copy marked those nullable — brain required `summary`/`project_tree`
 * outright. So the exact responses the `status` field exists to distinguish
 * were the ones that failed to parse, and the registry-down path rendered a
 * schema error rather than "the registry is unavailable".
 */
import { z } from 'zod';

/** One checkout inside a repo group (`project_registry.rs ProjectRegistryEntry`).
 * `kind` is `primary` | `worktree` | `project`; `is_active` is
 * `skip_serializing_if = "Option::is_none"`, so absent rather than null. */
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
    /** Graph mass (total nodes/edges across the project's stores). Optional
     * because the registry does not serve it yet: Brain's neurons size by
     * `store_count` until the backend lands mass telemetry, then upgrade
     * automatically. */
    graph_node_count: z.number().optional(),
    graph_edge_count: z.number().optional(),
  })
  .passthrough();
export type ProjectRegistryEntry = z.infer<typeof ProjectRegistryEntrySchema>;

/** One repository grouped by `git_common_dir` (`ProjectRepoGroup`).
 * `branches` is the union of branches across its checkouts. */
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

/** Registry rollup (`ProjectRegistrySummary`). */
export const ProjectRegistrySummarySchema = z
  .object({
    project_count: z.number(),
    repo_count: z.number(),
    truncated: z.boolean(),
  })
  .passthrough();
export type ProjectRegistrySummary = z.infer<typeof ProjectRegistrySummarySchema>;

/** One flat checkout row in `projects[]` (`PublicCodeProject`). */
export const PublicCodeProjectSchema = z
  .object({
    project_id: z.string(),
    label: z.string(),
    project_root: z.string(),
    display_root: z.string(),
    canonical_root: z.string(),
    git_common_dir: z.string().nullable().optional(),
    default_branch: z.string().nullable().optional(),
    created_at: z.number(),
    last_seen_at: z.number(),
    is_active: z.boolean().optional(),
  })
  .passthrough();
export type PublicCodeProject = z.infer<typeof PublicCodeProjectSchema>;

/**
 * Full `GET /api/projects` body.
 *
 * `status` is `ok` | `missing_registry` | `registry_unavailable`, typed as a
 * plain string on purpose: the producer writes those three as literals, and a
 * fourth added in Rust must arrive as an unfamiliar string this dashboard can
 * report rather than a parse failure that takes the page down.
 *
 * Every body field below `status` is nullable AND optional because the two
 * failure responses send each of them as an explicit `null`.
 */
export const ProjectsPayloadSchema = z
  .object({
    status: z.string(),
    /** Present on `registry_unavailable` only. */
    error: z.string().optional(),
    limit: z.number().nullable().optional(),
    truncated: z.boolean().nullable().optional(),
    active_project_id: z.string().nullable().optional(),
    active_project_root: z.string().nullable().optional(),
    summary: ProjectRegistrySummarySchema.nullable().optional(),
    project_tree: z.array(ProjectRepoGroupSchema).nullable().optional(),
    projects: z.array(PublicCodeProjectSchema).nullable().optional(),
  })
  .passthrough();
export type ProjectsPayload = z.infer<typeof ProjectsPayloadSchema>;

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

/**
 * Full `GET /api/projects/{project_id}` body (`projects.rs::context`).
 *
 * `status` is `ok` | `missing_registry` | `not_found` | `registry_unavailable`.
 * `aliases` and `stores` are `[]` rather than null on every failure path, so
 * they are optional but not nullable; `project` is null on all three.
 */
export const ProjectContextPayloadSchema = z
  .object({
    status: z.string(),
    /** Present on `registry_unavailable` only. */
    error: z.string().optional(),
    is_active: z.boolean().optional(),
    project: ProjectRegistryEntrySchema.nullable().optional(),
    aliases: z.array(ProjectAliasSchema).optional(),
    stores: z.array(ProjectStoreSchema).optional(),
  })
  .passthrough();
export type ProjectContextPayload = z.infer<typeof ProjectContextPayloadSchema>;
