/**
 * The holographic-memory reads the Knowledge workspace consumes beyond the
 * enveloped overview, status, and fact-detail routes.
 *
 * Each route answers a bare payload whose schema is generated from the Rust
 * handler's contract. The only local additions are cross-field invariants that
 * JSON Schema cannot state; a body that violates one did not come from its
 * handler and must fail the parse rather than render.
 */
import {
  MemoryOplogPayloadV1Schema,
  MemoryProjectionPayloadV1Schema,
  MemorySimilarityPayloadV1Schema,
  MemoryTrustHistoryPayloadV1Schema,
} from "../../contracts/generated.ts";
import { usePayload } from "./usePayload.ts";

/** The plugin mount every route below hangs off (`lib.rs` `project_api_router`). */
export const MEMORY_BASE = "/api/plugins/holographic";

/** `GET /fact/{id}/trust-history`: `partial` exactly when `next_after` names
 * the continuation, and never more events than the declared limit. */
export const TrustHistoryPayloadSchema = MemoryTrustHistoryPayloadV1Schema.superRefine(
  (payload, context) => {
    const partial = payload.completeness === "partial";
    if (partial !== (payload.next_after != null)) {
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
  },
);

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

/** `GET /projection`: coverage names the request limit and never examined
 * fewer rows than it returned. */
export const ProjectionPayloadSchema = MemoryProjectionPayloadV1Schema.superRefine(
  (payload, context) => {
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
  },
);

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

export function useMemorySimilarity(minSimilarity: number, limit = 25) {
  return usePayload(
    ["memory", "similarity", minSimilarity, limit],
    `${MEMORY_BASE}/similarity?min_similarity=${minSimilarity}&limit=${limit}`,
    MemorySimilarityPayloadV1Schema,
    { staleTime: 5 * 60_000 },
  );
}

export function useMemoryOplog(limit = 100) {
  return usePayload(
    ["memory", "oplog", limit],
    `${MEMORY_BASE}/oplog?limit=${limit}`,
    MemoryOplogPayloadV1Schema,
  );
}
