import { z } from 'zod';

/**
 * Wire-true shapes for the Loom weave.
 *
 * The endpoint recon behind these schemas (run against the real daemon on
 * 2026-07-25, `tracedecay dashboard --port 7341`) is what decides the whole
 * surface, so it is recorded here rather than in a commit message:
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
 *   GET /api/plugins/savings/sessions      → 200. THE thread source.
 *       Per session: session_id, provider, title, started_at, last_message_at,
 *       messages, is_subagent, models[]. This is the only endpoint that serves
 *       a per-session start time, so it is the weave's warp.
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
 * What is NOT served, and therefore is not drawn (see `WEFT_SOURCES` in
 * weave.ts): session↔commit correlation, session→file edits, branch/PR/CI
 * events. Those exist in the store but no dashboard route exposes them.
 */

/** One model's accounting inside a session row (savings_api.rs). Only `model`
 * is read by the weave; the token blocks stay unvalidated passthrough so a
 * daemon that grows a field cannot fail the decode. */
export const SessionModelSchema = z
  .object({ model: z.string().nullable().optional() })
  .passthrough();

/** One session row from `GET /api/plugins/savings/sessions`.
 *
 * `last_message_at` is nullable and, on the real profile, null for the large
 * majority of rows — a session's END is an unserved quantity for most of the
 * store. That nullability is load-bearing: it is what the weave draws as an
 * open thread rather than inventing a duration for. */
export const LoomSessionSchema = z
  .object({
    session_id: z.string(),
    provider: z.string(),
    title: z.string().nullable().optional(),
    started_at: z.number(),
    last_message_at: z.number().nullable().optional(),
    messages: z.number(),
    is_subagent: z.boolean().optional(),
    models: z.array(SessionModelSchema).optional(),
  })
  .passthrough();
export type LoomSession = z.infer<typeof LoomSessionSchema>;

/** Full `GET /api/plugins/savings/sessions` body. `available:false` is the
 * daemon's own typed "this store is not readable", distinct from an empty
 * `sessions` array, which means "readable and genuinely zero". */
export const LoomSessionsPayloadSchema = z
  .object({
    available: z.boolean().optional(),
    db: z.string().nullable().optional(),
    scope: z.string().optional(),
    range: z.string().optional(),
    since: z.number().optional(),
    /** Sessions in the whole store, which is far more than are returned. */
    total: z.number().optional(),
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
