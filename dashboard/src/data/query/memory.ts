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
 * local zod schema written against the handler — the same construction
 * `CurationConsole.tsx` uses for the read-only `/curation/{status,activity,runs}`
 * and daemon-settings routes.
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
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { z } from "zod";

import { fetchPayloadWrite, type PayloadWriteResult } from "./payload.ts";
import { payloadQueryKey, usePayload } from "./usePayload.ts";
import {
  scopeKey,
  scopeWritable,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from "../scope/store.ts";

/** The plugin mount every route below hangs off (`lib.rs` `project_api_router`). */
export const MEMORY_BASE = "/api/plugins/holographic";

/* ---- trust history ------------------------------------------------------- */

/**
 * How much of a feedback event this store can still account for.
 *
 * `memory_api::fact_trust_history_payload` maps
 * `ProjectMemoryFactFeedbackDetailsAvailabilityV1` onto these three words, and
 * the distinction is the whole point of the field: `legacy_redacted` is a row
 * whose detail was deliberately dropped by an older writer, `unknown` is a row
 * whose detail state was never recorded. Neither is "no detail" and neither may
 * render as blank.
 */
export const TrustDetailAvailabilitySchema = z.enum([
  "available",
  "legacy_redacted",
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
    timestamp: z.string(),
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

/**
 * The audit's own account of how complete it is.
 *
 * `processed`/`remaining` come off `ProjectMemoryFeedbackRepairProgressV1`, whose
 * `Unknown` and `NotRequired` variants report neither — so both are nullable,
 * and a surface that printed `0` for them would be inventing a measurement the
 * repair never took.
 */
export const TrustRepairSchema = z
  .object({
    state: z.enum(["unknown", "not_required", "complete", "incomplete"]),
    processed: z.number().nullable(),
    remaining: z.number().nullable(),
  })
  .passthrough();

/** `GET /fact/{id}/trust-history` (`memory_api::fact_trust_history`). */
export const TrustHistoryPayloadSchema = z
  .object({
    fact_id: z.number(),
    trust_history: z.array(TrustHistoryEventSchema),
    repair: TrustRepairSchema,
    error: z.string(),
  })
  .passthrough();
export type TrustHistoryPayload = z.infer<typeof TrustHistoryPayloadSchema>;

/**
 * One fact's trust audit, fetched only while that fact is open.
 *
 * Keyed by fact id so switching selection is a different cache entry rather
 * than a refetch into the previous fact's slot. `enabled` gates on a supplied
 * id: the route takes an `i64` path segment and would 404 on an empty one, and
 * a 404 is a reading this surface must not manufacture by asking a question it
 * has no subject for.
 */
export function useFactTrustHistory(factId: number | null) {
  return usePayload(
    ["memory", "trust-history", String(factId ?? "")],
    `${MEMORY_BASE}/fact/${encodeURIComponent(String(factId ?? 0))}/trust-history`,
    TrustHistoryPayloadSchema,
    { enabled: factId != null },
  );
}

/* ---- projection ---------------------------------------------------------- */

/**
 * One projected fact (`memory_service::projection::projection_point`).
 *
 * Every key in that `json!` is unconditional, with `unwrap_or` defaults, so
 * nothing here is optional. `metadata`, `bank_id` and `bank_name` fall back to
 * `Value::Null`, so they are nullable — a fact with no bank is a real reading.
 */
export const ProjectionPointSchema = z
  .object({
    fact_id: z.number(),
    x: z.number(),
    y: z.number(),
    category: z.string(),
    content: z.string(),
    trust_score: z.number(),
    retrieval_count: z.number(),
    created_at: z.number(),
    updated_at: z.number(),
    bank_name: z.string().nullable(),
    entity_count: z.number(),
    connection_count: z.number(),
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
    error: z.string(),
  })
  .passthrough();
export type ProjectionPayload = z.infer<typeof ProjectionPayloadSchema>;

/**
 * The 2D phase projection.
 *
 * The daemon caches this against the store's vector fingerprint and recomputes
 * on a blocking thread when it moves, so it is cheap on repeat and expensive
 * exactly once. A long `staleTime` keeps a workspace visit from paying that
 * cost per remount; it is a projection of the whole store, not a live reading.
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
    a_id: z.number(),
    b_id: z.number(),
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


/* ---- curation runs ------------------------------------------------------- */

/**
 * One ledger record (`AutomationRunLedgerRecord`), as the sidecar serializes it.
 *
 * `run_id`, `trigger`, `task`, `backend`, `status`, the application counts and the two
 * timestamps have no `skip_serializing_if`, so they are required. `model`,
 * `error`, `host_mode` and `fallback_status` all carry
 * `skip_serializing_if = "Option::is_none"` and are therefore ABSENT rather than
 * null when unset — `.optional().nullable()` would accept a null this writer
 * cannot emit and let a null-vs-absent regression through.
 */
export const CurationRunRecordSchema = z
  .object({
    run_id: z.string(),
    trigger: z.string(),
    task: z.string(),
    backend: z.string(),
    status: z.string(),
    reviewed_count: z.number(),
    accepted_count: z.number(),
    rejected_count: z.number(),
    skipped_count: z.number(),
    started_at: z.string(),
    completed_at: z.string(),
    model: z.string().optional(),
    host_mode: z.string().optional(),
    error: z.string().optional(),
    fallback_status: z.string().optional(),
    activation_policy: z.string().optional(),
    created_skills: z.array(z.unknown()).optional(),
    updated_skills: z.array(z.unknown()).optional(),
    applied_consolidations: z.array(z.unknown()).optional(),
    rejected_skills: z.array(z.unknown()).optional(),
    validation_repairs: z.array(z.unknown()).optional(),
    receipts: z.array(z.unknown()).optional(),
    llm_apply: z.unknown().optional(),
    curation_policy: z.unknown().optional(),
    deployment: z
      .object({
        status: z.enum(["complete", "partial_failure", "unavailable"]),
        exports: z.array(z.unknown()),
        materialization_scopes: z.array(z.unknown()),
        errors: z.array(z.string()),
        reason: z.string().optional(),
        retry_required: z.boolean(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();
export type CurationRunRecord = z.infer<typeof CurationRunRecordSchema>;

/**
 * `GET /curation/runs` (`memory_api::curation_runs`).
 *
 * A ledger that failed to load answers `{records: [], count: 0, …, error}` with
 * HTTP 200. `error` is therefore the only thing that tells an unreadable ledger
 * apart from a project that has never run automation, and the panel must read it
 * before it reads `records`.
 */
export const CurationRunsPayloadSchema = z
  .object({
    records: z.array(CurationRunRecordSchema),
    count: z.number(),
    limit: z.number(),
    error: z.string(),
  })
  .passthrough();
export type CurationRunsPayload = z.infer<typeof CurationRunsPayloadSchema>;

export function useCurationRuns(limit = 50) {
  return usePayload(
    ["memory", "curation", "runs", limit],
    `${MEMORY_BASE}/curation/runs?limit=${limit}`,
    CurationRunsPayloadSchema,
  );
}

/* ---- oplog --------------------------------------------------------------- */

/**
 * One memory operation's detail, in the three shapes the handler emits.
 *
 * `memory_api::oplog` switches over `ProjectMemoryDashboardOplogDetailsV1` and
 * writes a DIFFERENT object per variant: `{summary}` when the detail survives,
 * `{redacted: true}` when it was deliberately withheld, `{availability:
 * "unknown"}` when the store cannot say. A union rather than one loose object,
 * because those are three distinct domain states — `ready`, `redacted`,
 * `unknown` — and collapsing them into an optional `summary` would render a
 * privacy redaction and a missing record identically.
 */
export const OplogDetailSchema = z.union([
  z.object({ summary: z.string() }).passthrough(),
  z.object({ redacted: z.literal(true) }).passthrough(),
  z.object({ availability: z.literal("unknown") }).passthrough(),
]);
export type OplogDetail = z.infer<typeof OplogDetailSchema>;

/** One oplog row. `fact_id` is `target_legacy_fact_id`, which is `None` for an
 * operation with no legacy-addressable fact — serialized as null, not absent. */
export const OplogEventSchema = z
  .object({
    id: z.union([z.string(), z.number()]),
    ts: z.string(),
    op: z.string(),
    fact_id: z.number().nullable(),
    detail: OplogDetailSchema,
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

/* ---- curation config ----------------------------------------------------- */

/** One scheduled task's configuration (`AutomationTaskSettingsV1`). Every
 * optional duration is serialized as `null`, not omitted, by the domain
 * contract. */
export const AutomationTaskConfigSchema = z
  .object({
    enabled: z.boolean(),
    schedule: z.string().nullable(),
    interval_secs: z.number().nullable(),
    cooldown_secs: z.number().nullable(),
    min_idle_secs: z.number().nullable(),
    stale_lock_secs: z.number().nullable(),
  })
  .passthrough();
export type AutomationTaskConfigReading = z.infer<
  typeof AutomationTaskConfigSchema
>;

/** A resolved daemon-pinned `AutomationSettingsV1`. */
export const AutomationConfigSchema = z
  .object({
    schema_version: z.number(),
    enabled: z.boolean(),
    backend: z.string(),
    host_mode: z.string(),
    model_id: z.string().nullable(),
    timeout_secs: z.number(),
    scheduler_tick_secs: z.number(),
    combine_due_tasks: z.boolean(),
    allow_job_commands: z.boolean(),
    tasks: z
      .object({
        memory_curator: AutomationTaskConfigSchema,
        session_reflector: AutomationTaskConfigSchema,
        skill_writer: AutomationTaskConfigSchema,
      })
      .passthrough(),
  })
  .passthrough();
export type AutomationConfigReading = z.infer<typeof AutomationConfigSchema>;

/**
 * The project overlay, as `AutomationConfigPatch` serializes.
 *
 * Every member skips when `None`, so an overlay that sets nothing is `{}` and an
 * overlay that was never written at all is `null`. Both are real and different:
 * the first is a project file that overrides nothing, the second is a project
 * with no automation file. The config surface says which.
 */
export const AutomationConfigOverlaySchema = z.record(z.string(), z.unknown());

/** `GET|PATCH /curation/config` (`automation_config_api`). The daemon returns
 * the pinned revision together with the effective settings; no browser-side
 * global/project layering is authoritative. */
export const CurationConfigPayloadSchema = z
  .object({
    configuration_revision_id: z.string(),
    source: z.literal("daemon_pinned_snapshot"),
    effective: AutomationConfigSchema,
    backend_availability: z
      .object({
        backend: z.string(),
        available: z.boolean(),
        executable: z.string().nullable().optional(),
        reason: z.string().nullable().optional(),
      })
      .passthrough(),
    application_outcome: z.unknown().optional(),
  })
  .passthrough();
export type CurationConfigPayload = z.infer<typeof CurationConfigPayloadSchema>;

export const curationConfigKey = ["memory", "curation", "config"] as const;
export const curationConfigUrl = `${MEMORY_BASE}/curation/config`;

export function useCurationConfig() {
  return usePayload(
    curationConfigKey,
    curationConfigUrl,
    CurationConfigPayloadSchema,
  );
}

/**
/** The one setting this surface exposes. Validation and application policy are
 * daemon-owned and are never browser toggles. */
export interface CurationConfigPatch {
  readonly enabled: boolean;
}

export interface CurationConfigMutation extends CurationConfigPatch {
  readonly expected_revision_id: string;
  readonly idempotency_key: string;
}

export const CurationConfigMutationSchema = z
  .object({
    enabled: z.boolean(),
    expected_revision_id: z.string(),
    idempotency_key: z.string(),
  })
  .strict();

/**
 * What a config write produced, including the case where there was none.
 *
 * `not_dispatched` mirrors {@link import('./automation.ts').SchedulerControlResult}:
 * nothing was sent, so nothing changed, and the surface must not imply the
 * daemon was asked and refused.
 */
export type CurationConfigWriteResult =
  | PayloadWriteResult<CurationConfigPayload>
  | { outcome: "not_dispatched"; writability: ScopeWritability };

/** The scope a write was issued under, captured at dispatch — see
 * `useSchedulerControl`, where settling against the render's current scope
 * instead of the dispatch's wrote one project's answer into another's entry. */
interface ConfigDispatch {
  readonly configKey: readonly unknown[];
}

/**
 * The guarded config write.
 *
 * PATCH answers with the same payload GET does — the handler re-reads and
 * returns the resolved layering — so, exactly as with the scheduler control,
 * there is no optimistic update here: the server's own re-read is written into
 * the read's cache entry and the surface never shows a setting it has not
 * observed. A failed patch leaves the last real reading on screen.
 */
export function useCurationConfigPatch() {
  const scope = useScope((s) => s.scope);
  const client = useQueryClient();
  const configKey = payloadQueryKey(
    scope,
    curationConfigKey,
    curationConfigUrl,
  );
  const writability = scopeWritable(scope);
  const mutation = useMutation<
    CurationConfigWriteResult,
    Error,
    CurationConfigMutation,
    ConfigDispatch
  >({
    mutationKey: [...curationConfigKey, scopeKey(scope)],
    onMutate: () => ({ configKey }),
    mutationFn: async (patch: CurationConfigMutation) => {
      // Nothing leaves the browser unless the scope is known to accept it. The
      // control is disabled on this same reading, so arriving here means the
      // disable was bypassed, and dispatching anyway would trade a stated
      // reason for a 405 this layer cannot tell from a route that has gone away.
      if (writability.state !== "writable") {
        return { outcome: "not_dispatched", writability };
      }
      const parsed = CurationConfigMutationSchema.safeParse(patch);
      if (!parsed.success) {
        return {
          outcome: "error",
          detail: "invalid automation configuration mutation",
        };
      }
      return fetchPayloadWrite(
        scopedUrl(scope, curationConfigUrl),
        CurationConfigPayloadSchema,
        {
          method: "PATCH",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(patch),
        },
      );
    },
    onSuccess: (result, _patch, dispatch) => {
      const target = dispatch.configKey;
      if (result.outcome === "ok") {
        client.setQueryData(target, result);
        return;
      }
      if (result.outcome === "not_dispatched") return;
      void client.invalidateQueries({ queryKey: target });
    },
  });
  return { ...mutation, writability };
}
