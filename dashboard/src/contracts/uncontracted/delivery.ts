/**
 * UNCONTRACTED — `GET /api/delivery/overview`.
 *
 * Producer: `src/dashboard/delivery_api.rs`. Nothing here is generated; see
 * `./README.md`.
 *
 * This route serves the active checkout's bounded Git status/history and its
 * typed authority gaps. The registry side of Delivery — registered
 * repositories, indexed branches, and the primary/worktree checkouts that map
 * to each repository — comes from `GET /api/projects`, which lives in
 * `./projects.ts` and is shared with Brain rather than modelled again here.
 */
import { z } from 'zod';

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
  return z.union([z.object({ state: z.literal('ready'), value }), missingProjectionSchema]);
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
