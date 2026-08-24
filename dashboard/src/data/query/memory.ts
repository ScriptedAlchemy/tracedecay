/**
 * The holographic-memory reads the Knowledge workspace consumes beyond the
 * three contracted routes.
 *
 * `memory_api.rs` serves this family in two tiers. Three routes — `/`,
 * `/status`, `/fact/{id}` — answer `DashboardEnvelopeV1<…>` and are registered
 * in `contract_schema.rs`, so their schemas are generated and the workspace
 * reads them through {@link useEnvelope}. Every route below answers a bare
 * `Json<Value>`: they are NOT in the contract catalog, there is nothing for
 * codegen to emit, and the house ladder for that tier is `usePayload` plus a
 * local zod schema written against the handler.
 *
 * So these schemas are hand-written on purpose, and each one names the `json!`
 * literal it mirrors. Two rules follow from that provenance and are load-bearing
 * throughout:
 *
 *   - A key the handler emits unconditionally is REQUIRED here. Optional fields
 *     resolved through `?? []` are how a store the daemon could not read renders
 *     as a clean empty surface; a body missing an unconditional key did not come
 *     from this handler and must fail the parse.
 *   - A key the handler marks `skip_serializing_if = "Option::is_none"` is
 *     `.optional()` — genuinely absent, never `null`. A key it serializes as
 *     `null` is `.nullable()`. The two say different things about whether a
 *     measurement was taken, and this dashboard may not blur them.
 *
 * `.passthrough()` throughout: these handlers carry more than any one surface
 * reads, and a field added server-side must not fail an unrelated panel.
 */
import { z } from "zod";

import { usePayload } from "./usePayload.ts";

/** The plugin mount every route below hangs off (`lib.rs` `project_api_router`). */
export const MEMORY_BASE = "/api/plugins/holographic";

/* ---- trust history ------------------------------------------------------- */

/**
 * How much of a feedback event this store can still account for.
 *
 * `memory_api::fact_trust_history_payload` maps the canonical feedback-detail
 * availability directly. A redacted row has withheld detail; an unknown row
 * never recorded its detail state. Neither may render as blank.
 */
export const TrustDetailAvailabilitySchema = z.enum([
  "available",
  "redacted",
  "unknown",
]);
export type TrustDetailAvailability = z.infer<
  typeof TrustDetailAvailabilitySchema
>;

/**
 * One append-only feedback event.
 *
 * `timestamp`, `action`, `old_trust`, `new_trust`, `delta` and
 * `details_availability` are inserted unconditionally. `source` and `note` are
 * inserted only when the event carried them, so they are absent rather than
 * null — which is exactly the difference between "this event named no source"
 * and "this event's source is unknown".
 */
export const TrustHistoryEventSchema = z
  .object({
    event_id: z.string(),
    timestamp: z.number().int(),
    action: z.enum(["helpful", "unhelpful"]),
    old_trust: z.number(),
    new_trust: z.number(),
    delta: z.number(),
    details_availability: TrustDetailAvailabilitySchema,
    source: z.string().optional(),
    note: z.string().optional(),
  })
  .passthrough();
export type TrustHistoryEvent = z.infer<typeof TrustHistoryEventSchema>;

/** `GET /fact/{id}/trust-history` (`memory_api::fact_trust_history`). */
export const TrustHistoryPayloadSchema = z
  .object({
    fact_id: z.string(),
    trust_history: z.array(TrustHistoryEventSchema),
    limit: z.number().int().positive(),
    completeness: z.enum(["complete", "partial"]),
    next_after: z
      .object({
        occurred_at: z.number().int(),
        event_id: z.string(),
      })
      .strict()
      .nullable(),
    error: z.string(),
  })
  .passthrough()
  .superRefine((payload, context) => {
    const partial = payload.completeness === "partial";
    if (partial !== (payload.next_after !== null)) {
      context.addIssue({
        code: "custom",
        path: ["next_after"],
        message: "trust-history completeness contradicts its continuation",
      });
    }
    if (payload.trust_history.length > payload.limit) {
      context.addIssue({
        code: "custom",
        path: ["trust_history"],
        message: "trust history exceeds its declared limit",
      });
    }
  });
export type TrustHistoryPayload = z.infer<typeof TrustHistoryPayloadSchema>;

/**
 * One fact's trust audit, fetched only while that fact is open.
 *
 * Keyed by fact id so switching selection is a different cache entry rather
 * than a refetch into the previous fact's slot. `enabled` gates on a supplied
 * id: the route takes a canonical string identity and rejects an empty one, and
 * a 404 is a reading this surface must not manufacture by asking a question it
 * has no subject for.
 */
export function useFactTrustHistory(factId: string | null) {
  return usePayload(
    ["memory", "trust-history", String(factId ?? "")],
    `${MEMORY_BASE}/fact/${encodeURIComponent(factId ?? "")}/trust-history`,
    TrustHistoryPayloadSchema,
    { enabled: factId != null },
  );
}

/* ---- projection ---------------------------------------------------------- */

/**
 * One projected fact (`memory_service::projection::projection_point`).
 *
 * Every key in the projection payload is unconditional, so nothing here is
 * optional.
 */
export const ProjectionPointSchema = z
  .object({
    fact_id: z.string(),
    payload_access: z.literal("eligible"),
    x: z.number(),
    y: z.number(),
    category: z.string(),
    content: z.string(),
    trust_score: z.number(),
    retrieval_count: z.number(),
    access_count: z.number(),
    helpful_count: z.number(),
    unhelpful_count: z.number(),
    created_at: z.number(),
    updated_at: z.number(),
    projected_as_of: z.number(),
    last_recalled_at: z.number().nullable(),
    tags: z.array(z.string()),
    entities: z.array(z.string()),
    metadata: z.unknown(),
    source_label: z.string().optional(),
    entity_count: z.number(),
  })
  .passthrough();
export type ProjectionPoint = z.infer<typeof ProjectionPointSchema>;

/**
 * `GET /projection` (`memory_service::projection_payload`).
 *
 * `method` is the honest part of this payload. The handler emits `"pca"` only
 * when `pca_scores` succeeded over at least two equal-length phase vectors;
 * everything else — one point, no vectors, a failed decomposition — is
 * `"none"`, and a `none` scatter is not a map of the store's semantic space. A
 * surface that drew both the same way would be claiming a projection the daemon
 * explicitly declined to compute.
 */
export const ProjectionPayloadSchema = z
  .object({
    exists: z.boolean(),
    dim: z.number(),
    limit: z.number(),
    method: z.string(),
    points: z.array(ProjectionPointSchema),
    coverage: z
      .object({
        completeness: z.enum(["complete", "bounded", "unknown"]),
        examined: z.number().int().nonnegative(),
        limit: z.number().int().positive(),
        omission_reasons: z.array(z.string()),
      })
      .strict(),
    error: z.string(),
  })
  .passthrough()
  .superRefine((payload, context) => {
    if (payload.coverage.limit !== payload.limit) {
      context.addIssue({
        code: "custom",
        path: ["coverage", "limit"],
        message: "projection coverage limit contradicts the request limit",
      });
    }
    if (payload.points.length > payload.coverage.examined) {
      context.addIssue({
        code: "custom",
        path: ["points"],
        message: "projection returned more points than it examined",
      });
    }
  });
export type ProjectionPayload = z.infer<typeof ProjectionPayloadSchema>;

/**
 * The 2D phase projection.
 *
 * The daemon caches this bounded projection against the store's vector
 * fingerprint and recomputes on a blocking thread when it moves, so it is
 * cheap on repeat and expensive exactly once. A long `staleTime` keeps a
 * workspace visit from paying that cost per remount. The current payload has
 * no whole-store denominator or continuation, so callers must keep its
 * coverage unknown even when the returned page is empty.
 */
export function useMemoryProjection(query: string, limit = 400) {
  const search =
    query.trim() === "" ? "" : `&q=${encodeURIComponent(query.trim())}`;
  return usePayload(
    ["memory", "projection", query.trim(), limit],
    `${MEMORY_BASE}/projection?limit=${limit}${search}`,
    ProjectionPayloadSchema,
    { staleTime: 5 * 60_000 },
  );
}

/* ---- similarity ---------------------------------------------------------- */

/** One scored pair (`memory_service::similarity_payload`). The overlap block is
 * merged in from `scored_pair.overlap`, whose members vary by classifier, and is
 * therefore left to `.passthrough()` rather than guessed at here. */
export const SimilarityPairSchema = z
  .object({
    a_id: z.string(),
    b_id: z.string(),
    a_content: z.string(),
    b_content: z.string(),
    a_category: z.string(),
    b_category: z.string(),
    similarity: z.number(),
    classification: z.string(),
  })
  .passthrough();
export type SimilarityPair = z.infer<typeof SimilarityPairSchema>;

/**
 * The distribution `memory_analysis::score_distribution` computes.
 *
 * Every statistic is nullable because the handler emits `Value::Null` for all of
 * them when no finite pair was scored. That is the one case this payload must
 * not be read as "the average similarity is zero".
 */
export const SimilarityDistributionSchema = z
  .object({
    min_score: z.number().nullable(),
    max_score: z.number().nullable(),
    average_score: z.number().nullable(),
    bin_count: z.number(),
    total_pairs: z.number(),
    bins: z.array(
      z
        .object({ start: z.number(), end: z.number(), count: z.number() })
        .passthrough(),
    ),
  })
  .passthrough();
export type SimilarityDistribution = z.infer<
  typeof SimilarityDistributionSchema
>;

/**
 * `GET /similarity` (`memory_api::similarity`).
 *
 * `count` is the number of VECTORED facts the computation ran over, not the
 * store's fact total, and `total_pairs` is the number of scored pairs before the
 * floor and the cap are applied. `pairs` is what survived both. Three different
 * denominators, all emitted, and the panel prints them apart.
 */
export const SimilarityPayloadSchema = z
  .object({
    exists: z.boolean(),
    dim: z.number(),
    count: z.number(),
    limit: z.number(),
    min_similarity: z.number(),
    total_pairs: z.number(),
    score_distribution: SimilarityDistributionSchema,
    pairs: z.array(SimilarityPairSchema),
    error: z.string(),
  })
  .passthrough();
export type SimilarityPayload = z.infer<typeof SimilarityPayloadSchema>;

export function useMemorySimilarity(minSimilarity: number, limit = 25) {
  return usePayload(
    ["memory", "similarity", minSimilarity, limit],
    `${MEMORY_BASE}/similarity?min_similarity=${minSimilarity}&limit=${limit}`,
    SimilarityPayloadSchema,
    { staleTime: 5 * 60_000 },
  );
}

/* ---- oplog --------------------------------------------------------------- */

/** One canonical lineage operation. Operations without a fact target serialize
 * `fact_id` as null; the route does not expose mutation detail. */
export const OplogEventSchema = z
  .object({
    id: z.number().int(),
    ts: z.number().int(),
    op: z.string(),
    fact_id: z.string().nullable(),
  })
  .passthrough();
export type OplogEvent = z.infer<typeof OplogEventSchema>;

/** `GET /oplog` (`memory_service::oplog_payload`). Same 200-with-`error`
 * construction as the runs ledger. */
export const OplogPayloadSchema = z
  .object({
    events: z.array(OplogEventSchema),
    count: z.number(),
    limit: z.number(),
    error: z.string(),
  })
  .passthrough();
export type OplogPayload = z.infer<typeof OplogPayloadSchema>;

export function useMemoryOplog(limit = 100) {
  return usePayload(
    ["memory", "oplog", limit],
    `${MEMORY_BASE}/oplog?limit=${limit}`,
    OplogPayloadSchema,
  );
}
