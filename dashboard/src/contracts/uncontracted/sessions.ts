/**
 * UNCONTRACTED — the session/thread routes.
 *
 *   GET /api/plugins/savings/sessions          (session rows)
 *   GET /api/plugins/hermes-lcm/session/{id}   (message chain)
 *   GET /api/plugins/hermes-lcm/timeline       (day buckets)
 *   GET /api/loom/temporal                     (threads + causal relations)
 *
 * Producers: `src/dashboard/savings_api.rs`, `src/dashboard/lcm_api.rs`,
 * `src/dashboard/loom_api.rs`. Nothing here is generated; see `./README.md`.
 *
 * These four live in one module because they share the session row: Loom's
 * temporal projection and the savings session list return the same shape, and
 * splitting them by workspace is how one route ends up modelled twice.
 * The names are the ROUTE's, not a workspace's — the Sessions workspace reads
 * `timeline` too, and `LoomSession` was never Loom's to own. Only the
 * `/api/loom/temporal` members keep a `Loom` prefix, because that is the
 * route's own name.
 *
 * What the endpoint boundaries permit each surface to claim:
 *
 *   GET /api/plugins/hermes-lcm/overview   → HTTP 500, persistently.
 *       The handler (`lcm_api.rs::overview`) computes a perfectly good overview
 *       payload and then decorates it with a `payload_health` probe for a
 *       hardcoded "cursor" provider; when that probe fails the `?` discards the
 *       whole response. On this profile it always fails ("migration SQL query
 *       materialization exceeded its limit"). Loom therefore does not read
 *       `overview` at all — a surface may not be hostage to an enrichment field
 *       it never draws. It is deliberately absent from this module.
 *
 *   GET /api/loom/temporal                 → THE thread and causal source.
 *       Sessions include recorded ends plus message/model rollups. Commit
 *       attributions, edited-file rollups and branch/worktree spans retain
 *       provider, source granularity and explicit coverage.
 *
 *   GET /api/plugins/hermes-lcm/session/{id} → 200. THE chain source.
 *       Per message: ordinal, role, tool_name, token_estimate, content.
 *       `timestamp` is served but is null on every message of every session
 *       sampled — the chain is therefore ordinal-ordered, never time-ordered,
 *       and the surface says so.
 *
 *   GET /api/plugins/hermes-lcm/timeline   → 200. Day buckets {count,
 *       token_estimate}: real message density to rule the time axis against.
 *
 * PR/review/CI/release outcomes remain Delivery-owned. Until
 * `GET /api/delivery/overview` exposes session-linked rows, Loom receives a
 * typed unsupported status naming that exact required authority.
 */
import { z } from 'zod';

import { EnvelopeSchema } from '../generated.ts';

/** One model named by a session row. */
export const SessionModelSchema = z
  .object({ model: z.string().nullable().optional() })
  .passthrough();

/** One session row, served by both `/api/plugins/savings/sessions` and the
 * `sessions[]` of `/api/loom/temporal`.
 *
 * `started_at` is nullable because the all-range backend deliberately includes
 * sessions with no usable timestamp. `last_message_at` is also nullable and,
 * on the real profile, null for the large majority of rows. Both distinctions
 * are load-bearing: undated rows are counted but not placed, while a dated row
 * without an end is drawn open rather than assigned an invented duration. */
export const SessionRowSchema = z
  .object({
    session_id: z.string(),
    provider: z.string(),
    title: z.string().nullable().optional(),
    started_at: z.number().nullable(),
    ended_at: z.number().nullable().optional(),
    last_message_at: z.number().nullable().optional(),
    messages: z.number().int().nonnegative(),
    is_subagent: z.boolean().optional(),
    edited_files_recorded: z.boolean().optional(),
    models: z.array(SessionModelSchema).optional(),
  })
  .passthrough();
export type SessionRow = z.infer<typeof SessionRowSchema>;

/** Full `GET /api/plugins/savings/sessions` body. Retained for shared endpoint
 * fixture coverage; Loom itself reads `LoomTemporalPayloadSchema`. */
export const SessionsPayloadSchema = z
  .object({
    available: z.boolean().optional(),
    db: z.string().nullable().optional(),
    scope: z.string().optional(),
    range: z.string().optional(),
    since: z.number().optional(),
    total: z.number().int().nonnegative().optional(),
    sessions: z.array(SessionRowSchema).optional(),
  })
  .passthrough();
export type SessionsPayload = z.infer<typeof SessionsPayloadSchema>;

/** One message from `GET /api/plugins/hermes-lcm/session/{id}`.
 *
 * `timestamp` is typed here because the daemon sends the key, and typed as
 * nullable because it is null in practice — the chain view reads it, finds
 * nothing, and prints that rather than silently falling back to ordinal while
 * implying time. */
export const ChainMessageSchema = z
  .object({
    message_id: z.string(),
    role: z.string().nullable().optional(),
    content: z.string().nullable().optional(),
    ordinal: z.number().nullable().optional(),
    timestamp: z.number().nullable().optional(),
    tool_name: z.string().nullable().optional(),
    token_estimate: z.number().nullable().optional(),
  })
  .passthrough();
export type ChainMessage = z.infer<typeof ChainMessageSchema>;

/** Full `GET /api/plugins/hermes-lcm/session/{id}` body. */
export const ChainPayloadSchema = z
  .object({
    exists: z.boolean().optional(),
    session_id: z.string().optional(),
    has_more_messages: z.boolean().optional(),
    counts: z
      .object({
        message_count: z.number().optional(),
        token_estimate_total: z.number().optional(),
        summary_node_count: z.number().optional(),
      })
      .passthrough()
      .optional(),
    messages: z.array(ChainMessageSchema).optional(),
  })
  .passthrough();
export type ChainPayload = z.infer<typeof ChainPayloadSchema>;

/** One day bucket from `GET /api/plugins/hermes-lcm/timeline`. */
export const TimelineBucketSchema = z
  .object({
    bucket: z.string(),
    count: z.number(),
    token_estimate: z.number().optional(),
  })
  .passthrough();
export type TimelineBucket = z.infer<typeof TimelineBucketSchema>;

/** Full `GET /api/plugins/hermes-lcm/timeline` body. */
export const TimelinePayloadSchema = z
  .object({
    bucket: z.string().optional(),
    buckets: z.array(TimelineBucketSchema).optional(),
  })
  .passthrough();
export type TimelinePayload = z.infer<typeof TimelinePayloadSchema>;

/* --- GET /api/loom/temporal ------------------------------------------------ */

export const LoomSourceIdSchema = z.enum([
  'session_commit',
  'session_file',
  'branch_worktree',
  'delivery_outcomes',
]);
export type LoomSourceId = z.infer<typeof LoomSourceIdSchema>;

const LoomReadStateSchema = z.enum(['ready', 'partial', 'unknown', 'unsupported']);

export const LoomSourceStatusSchema = z.object({
  id: LoomSourceIdSchema,
  label: z.string(),
  state: LoomReadStateSchema,
  authority: z.string().nullable(),
  granularity: z.string(),
  providers: z.array(z.string()),
  item_count: z.number().int().nonnegative().nullable(),
  reason: z.string().nullable(),
  required_authority: z.string().nullable(),
  coverage: z.object({
    completeness: z.enum(['complete', 'partial', 'unknown', 'unsupported']),
    eligible: z.number().int().nonnegative().nullable(),
    examined: z.number().int().nonnegative().nullable(),
    matched: z.number().int().nonnegative().nullable(),
    omitted: z.number().int().nonnegative().nullable(),
    unit: z.string().nullable(),
    reason: z.string(),
  }),
});
export type LoomSourceStatus = z.infer<typeof LoomSourceStatusSchema>;

export const LoomCommitSchema = z.object({
  provider: z.string(),
  session_id: z.string(),
  commit_sha: z.string(),
  committed_at: z.number().int(),
  branch: z.string().nullable(),
  worktree: z.string().nullable(),
  relation: z.string(),
  evidence: z.string(),
  confidence: z.number().min(0).max(100),
  span_overlap_kind: z.string().nullable(),
});
export type LoomCommit = z.infer<typeof LoomCommitSchema>;

export const LoomEditedFileSchema = z.object({
  provider: z.string(),
  session_id: z.string(),
  path: z.string(),
  change_type: z.string().nullable(),
  hunks: z.number().int().nonnegative().nullable(),
});
export type LoomEditedFile = z.infer<typeof LoomEditedFileSchema>;

export const LoomBranchSpanSchema = z
  .object({
    provider: z.string(),
    session_id: z.string(),
    branch: z.string().nullable(),
    worktree: z.string(),
    first_at: z.number().int(),
    last_at: z.number().int(),
    event_count: z.number().int().positive(),
    source: z.string(),
  })
  .refine((span) => span.last_at >= span.first_at, {
    message: 'branch span last_at must not precede first_at',
  });
export type LoomBranchSpan = z.infer<typeof LoomBranchSpanSchema>;

export const LoomTemporalRefreshSchema = z.object({
  state: LoomReadStateSchema,
  active_generations: z.number().int().nonnegative(),
  latest_activated_at_micros: z.number().int().nullable(),
  authority: z.string(),
});
export type LoomTemporalRefresh = z.infer<typeof LoomTemporalRefreshSchema>;

export const LoomTemporalPayloadSchema = z.object({
  available: z.boolean(),
  total: z.number().int().nonnegative(),
  sessions: z.array(SessionRowSchema),
  source_statuses: z.array(LoomSourceStatusSchema),
  commits: z.array(LoomCommitSchema),
  edited_files: z.array(LoomEditedFileSchema),
  branch_spans: z.array(LoomBranchSpanSchema),
  temporal_refresh: LoomTemporalRefreshSchema,
});
export type LoomTemporalPayload = z.infer<typeof LoomTemporalPayloadSchema>;

/** The one place an uncontracted payload is carried inside the CONTRACTED
 * envelope: the envelope itself is generated, the payload is not. */
export const LoomTemporalEnvelopeSchema = EnvelopeSchema(LoomTemporalPayloadSchema);
