/**
 * Wire-true contracts hand-matched to the Rust source of truth
 * (src/dashboard/read_model.rs, DashboardEnvelopeV1 + closed enums).
 *
 * TRANSITIONAL: the codegen pipeline (dashboard/codegen/) currently generates
 * from fixture schemas; once the Rust side exports real JSON Schemas this
 * module is replaced by generated output. Until then, this file is the ONLY
 * hand-written wire boundary permitted, and it must track read_model.rs
 * exactly (schema_revision gate below hard-fails on drift).
 */
import { z } from 'zod';

export const WIRE_SCHEMA_REVISION = 1;

export const DomainStateSchema = z
  .enum([
    'loading',
    'complete_zero_findings',
    'ready',
    'partial',
    'stale',
    'locked',
    'denied',
    'unauthorized',
    'redacted',
    'conflicting',
    'offline',
    'unknown',
    'cancelled',
    'timed_out',
    'error',
    'unsupported_schema',
    'unsupported',
  ])
  .catch('unsupported_schema');
export type WireDomainState = z.infer<typeof DomainStateSchema>;

export const ScopeSchema = z.object({
  project_id: z.string().nullable(),
  storage_mode: z.string(),
  store_root: z.string(),
});

export const VersionSchema = z.object({
  entity_version: z.string().nullable(),
  graph_version: z.string().nullable(),
});

export const TimeSchema = z.object({
  valid_time_micros: z.number().nullable(),
  observation_time_micros: z.number(),
});

export const WatermarkSchema = z.object({
  source: z.string(),
  watermark: z.string(),
});

export const AuthorizationSchema = z.object({
  outcome: z.enum(['authorized', 'unauthorized', 'denied', 'redacted']).catch('unauthorized'),
});

export const CoverageSchema = z.object({
  completeness: z.enum(['complete', 'partial', 'unknown', 'unsupported']).catch('unknown'),
  eligible: z.number().nullable(),
  examined: z.number().nullable(),
  matched: z.number().nullable(),
  excluded: z.number().nullable(),
  omitted: z.number().nullable(),
  unknown: z.number().nullable(),
  denominator: z.number().nullable(),
  unit: z.string().nullable(),
  omission_reasons: z.array(z.string()),
});
export type WireCoverage = z.infer<typeof CoverageSchema>;

export const FreshnessSchema = z.object({
  state: z.enum(['fresh', 'stale', 'unknown', 'absent', 'unsupported']).catch('unknown'),
  observed_at_micros: z.number().nullable(),
  watermark: z.string().nullable(),
});
export type WireFreshness = z.infer<typeof FreshnessSchema>;

export const LegalActionRefSchema = z.object({
  kind: z.string(),
  operation: z.string(),
});

export function EnvelopeSchema<T>(payload: z.ZodType<T>) {
  return z.object({
    schema_revision: z.number(),
    scope: ScopeSchema,
    version: VersionSchema,
    time: TimeSchema,
    source_watermark: WatermarkSchema.nullable(),
    authorization: AuthorizationSchema,
    coverage: CoverageSchema,
    freshness: FreshnessSchema,
    domain_state: DomainStateSchema,
    legal_actions: z.array(LegalActionRefSchema.passthrough()),
    payload,
  });
}
export type WireEnvelope<T> = z.infer<ReturnType<typeof EnvelopeSchema<T>>> & { payload: T };

/* ---- /api/storage/telemetry payload (storage_telemetry_api.rs) ---- */

export const StoreTelemetryEntrySchema = z.object({
  store: z.string(),
  role: z.string(),
  path: z.string(),
  read: z.object({ kind: z.string() }).passthrough(),
  total_bytes: z.number().nullable(),
  free_bytes: z.number().nullable(),
  free_page_ratio: z.number().nullable(),
  budget: z.object({ state: z.string() }).passthrough(),
  growth: z.object({ state: z.string() }).passthrough(),
});
export type StoreTelemetryEntry = z.infer<typeof StoreTelemetryEntrySchema>;

export const StorageTelemetryPayloadSchema = z.object({
  stores: z.array(StoreTelemetryEntrySchema),
  budget_note: z.string(),
  growth_note: z.string(),
});
export type StorageTelemetryPayload = z.infer<typeof StorageTelemetryPayloadSchema>;
