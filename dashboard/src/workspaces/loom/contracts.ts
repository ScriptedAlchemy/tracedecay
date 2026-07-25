import { z } from 'zod';
import { EnvelopeSchema } from '../../contracts/wire.ts';

/**
 * Wire-true shapes for the Loom weave.
 *
 * The endpoint boundaries behind these schemas decide what the surface may
 * claim:
 *
 *   GET /api/plugins/hermes-lcm/overview   → HTTP 500, persistently.
 *       The handler (src/dashboard/lcm_api.rs::overview) computes a perfectly
 *       good overview payload and then decorates it with a `payload_health`
 *       probe for a hardcoded "cursor" provider; when that probe fails the
 *       `?` discards the whole response. On this profile it always fails
 *       ("migration SQL query materialization exceeded its limit"). Loom
 *       therefore does not read `overview` at all — a surface may not be
 *       hostage to an enrichment field it never draws.
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

/** One model named by the Loom temporal projection. */
export const SessionModelSchema = z
  .object({ model: z.string().nullable().optional() })
  .passthrough();

/** One session row from `GET /api/loom/temporal`.
 *
 * `started_at` is nullable because the all-range backend deliberately includes
 * sessions with no usable timestamp. `last_message_at` is also nullable and,
 * on the real profile, null for the large majority of rows. Both distinctions
 * are load-bearing: undated rows are counted but not placed, while a dated row
 * without an end is drawn open rather than assigned an invented duration. */
export const LoomSessionSchema = z
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
export type LoomSession = z.infer<typeof LoomSessionSchema>;

/** Legacy savings-session decoder retained for shared endpoint fixture
 * coverage. Loom itself reads `LoomTemporalPayloadSchema`. */
export const LoomSessionsPayloadSchema = z
  .object({
    available: z.boolean().optional(),
    db: z.string().nullable().optional(),
    scope: z.string().optional(),
    range: z.string().optional(),
    since: z.number().optional(),
    total: z.number().int().nonnegative().optional(),
    sessions: z.array(LoomSessionSchema).optional(),
  })
  .passthrough();
export type LoomSessionsPayload = z.infer<typeof LoomSessionsPayloadSchema>;

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
export const LoomChainPayloadSchema = z
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
export type LoomChainPayload = z.infer<typeof LoomChainPayloadSchema>;

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
export const LoomTimelinePayloadSchema = z
  .object({
    bucket: z.string().optional(),
    buckets: z.array(TimelineBucketSchema).optional(),
  })
  .passthrough();
export type LoomTimelinePayload = z.infer<typeof LoomTimelinePayloadSchema>;

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

export const LoomBranchSpanSchema = z.object({
  provider: z.string(),
  session_id: z.string(),
  branch: z.string().nullable(),
  worktree: z.string(),
  first_at: z.number().int(),
  last_at: z.number().int(),
  event_count: z.number().int().positive(),
  source: z.string(),
}).refine((span) => span.last_at >= span.first_at, {
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
  sessions: z.array(LoomSessionSchema),
  source_statuses: z.array(LoomSourceStatusSchema),
  commits: z.array(LoomCommitSchema),
  edited_files: z.array(LoomEditedFileSchema),
  branch_spans: z.array(LoomBranchSpanSchema),
  temporal_refresh: LoomTemporalRefreshSchema,
});
export type LoomTemporalPayload = z.infer<typeof LoomTemporalPayloadSchema>;

export const LoomTemporalEnvelopeSchema = EnvelopeSchema(LoomTemporalPayloadSchema);
