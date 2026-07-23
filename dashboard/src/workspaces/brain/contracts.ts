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
    status: z.string(),
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
