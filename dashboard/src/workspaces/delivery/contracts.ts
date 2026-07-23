import { z } from 'zod';

/** Wire-true shapes for `GET /api/projects`
 * (src/dashboard/projects.rs::list → serialized from src/project_registry.rs).
 *
 * This is the only delivery-relevant git surface the daemon exposes to the
 * dashboard: registered repositories, the branches indexed under each, and the
 * primary/worktree checkouts that map to them. Commit history, pull-request,
 * and code-review state are NOT served over the dashboard API (there is no
 * advisory/PR/review route in src/dashboard/mod.rs), and per-worktree index
 * freshness is typed `unsupported` at src/dashboard/code_index_freshness_api.rs.
 * DeliveryPage renders those as truthful typed-unavailable states rather than
 * inventing data. */

/** One checkout as returned in `projects[]` (src/project_registry.rs
 * PublicCodeProject). `default_branch`/`git_common_dir` serialize as null when
 * absent; `is_active` is skipped when unknown. */
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

/** One checkout inside a repo group (src/project_registry.rs
 * ProjectRegistryEntry). `kind` is `primary` | `worktree` | `project`. */
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
  })
  .passthrough();
export type ProjectRegistryEntry = z.infer<typeof ProjectRegistryEntrySchema>;

/** One repository grouped by `git_common_dir` (src/project_registry.rs
 * ProjectRepoGroup). `branches` is the union of branches across its checkouts. */
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

/** Registry rollup (src/project_registry.rs ProjectRegistrySummary). */
export const ProjectRegistrySummarySchema = z
  .object({
    project_count: z.number(),
    repo_count: z.number(),
    truncated: z.boolean(),
  })
  .passthrough();
export type ProjectRegistrySummary = z.infer<typeof ProjectRegistrySummarySchema>;

/** Full `GET /api/projects` body. `status` is `ok` or `missing_registry`
 * (empty tree/projects when the savings registry is unavailable). */
export const DeliveryProjectsPayloadSchema = z
  .object({
    status: z.string(),
    limit: z.number().optional(),
    truncated: z.boolean().optional(),
    active_project_id: z.string().nullable().optional(),
    active_project_root: z.string().nullable().optional(),
    summary: ProjectRegistrySummarySchema.optional(),
    project_tree: z.array(ProjectRepoGroupSchema).optional(),
    projects: z.array(PublicCodeProjectSchema).optional(),
  })
  .passthrough();
export type DeliveryProjectsPayload = z.infer<typeof DeliveryProjectsPayloadSchema>;
