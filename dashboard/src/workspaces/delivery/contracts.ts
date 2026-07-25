import { z } from 'zod';

/** Wire-true shapes for `GET /api/projects`
 * (src/dashboard/projects.rs::list → serialized from src/project_registry.rs).
 *
 * It serves registered repositories, indexed branch names, and the
 * primary/worktree checkouts that map to each repository. The separate
 * `/api/delivery/overview` envelope serves the active checkout's bounded Git
 * status/history and typed authority gaps. */

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

const GitHeadSchema = z.discriminatedUnion('state', [
  z.object({ state: z.literal('attached'), branch: z.string(), commit: z.string() }),
  z.object({ state: z.literal('detached'), commit: z.string() }),
  z.object({ state: z.literal('unborn'), branch: z.string() }),
]);

export const DeliveryChangesSchema = z.object({
  head: GitHeadSchema,
  staged: z.number(),
  unstaged: z.number(),
  conflicted: z.number(),
  untracked: z.number(),
  ignored: z.number(),
  changed_paths: z.array(z.string()),
});

export const DeliveryCommitSchema = z.object({
  commit: z.string(),
  subject: z.string(),
  author_name: z.string(),
  author_email: z.string(),
  author_at_micros: z.number(),
  committer_at_micros: z.number(),
});

export const DeliveryCommitsSchema = z.object({
  items: z.array(DeliveryCommitSchema),
  truncated: z.boolean(),
});

export const DeliveryGenerationFreshnessSchema = z.object({
  comparison: z.enum(['current', 'behind']),
  head_commit: z.string(),
  indexed_commit: z.string(),
});

const missingProjectionSchema = z.object({
  state: z.enum(['unavailable', 'unsupported']),
  required_authority: z.string(),
  reason: z.string(),
});

function projectionSchema<T extends z.ZodTypeAny>(value: T) {
  return z.union([
    z.object({ state: z.literal('ready'), value }),
    missingProjectionSchema,
  ]);
}

/** Reusable Delivery/Loom projection envelope payload. Unmounted authorities
 * carry no `value`; consumers must render their typed state and reason. */
export const DeliveryOverviewPayloadSchema = z.object({
  changes: projectionSchema(DeliveryChangesSchema),
  commits: projectionSchema(DeliveryCommitsSchema),
  pull_requests: projectionSchema(z.object({ items: z.array(z.unknown()) })),
  review_comments: projectionSchema(z.object({ items: z.array(z.unknown()) })),
  ci_checks: projectionSchema(z.object({ items: z.array(z.unknown()) })),
  failure_localization: projectionSchema(z.object({ items: z.array(z.unknown()) })),
  releases: projectionSchema(z.object({ items: z.array(z.unknown()) })),
  generation_freshness: projectionSchema(DeliveryGenerationFreshnessSchema),
});
export type DeliveryOverviewPayload = z.infer<typeof DeliveryOverviewPayloadSchema>;
