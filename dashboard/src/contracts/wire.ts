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
  outcome: z.enum(['authorized', 'unauthorized', 'denied', 'redacted']),
});

export const CoverageSchema = z.object({
  completeness: z.enum(['complete', 'partial', 'unknown', 'unsupported']),
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
  state: z.enum(['fresh', 'stale', 'unknown', 'absent', 'unsupported']),
  observed_at_micros: z.number().nullable(),
  watermark: z.string().nullable(),
});
export type WireFreshness = z.infer<typeof FreshnessSchema>;

export const LegalActionKindSchema = z.enum([
  'inspect',
  'expand_evidence',
  'refresh',
  'request_dry_run',
  'request_apply',
  'request_cancel',
]);

export const LegalActionRefSchema = z.object({
  kind: LegalActionKindSchema,
  operation: z.string(),
});
export type WireLegalActionRef = z.infer<typeof LegalActionRefSchema>;

export function EnvelopeSchema<T>(payload: z.ZodType<T>) {
  return z.object({
    schema_revision: z.literal(WIRE_SCHEMA_REVISION),
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

const StoreSizeSampleSchema = z.object({
  store: z.string(),
  page_size_bytes: z.number(),
  page_count: z.number(),
  freelist_pages: z.number(),
  observed_at: z.number(),
});

export const StorageTelemetryReadSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('observed'), sample: StoreSizeSampleSchema }),
  z.object({ kind: z.literal('unsupported'), store: z.string() }),
  z.object({ kind: z.literal('denied'), store: z.string() }),
  z.object({ kind: z.literal('unknown'), store: z.string() }),
]);
export type StorageTelemetryRead = z.infer<typeof StorageTelemetryReadSchema>;

const StoreBudgetDimensionSchema = z.discriminatedUnion('state', [
  z.object({
    state: z.literal('unsupported'),
    reason: z.string(),
  }),
]);

const TableGrowthSampleSchema = z.object({
  store: z.string(),
  table: z.string(),
  previous_bytes: z.number(),
  current_bytes: z.number(),
  previous_observed_at: z.number(),
  current_observed_at: z.number(),
});

const StoreGrowthDimensionSchema = z.discriminatedUnion('state', [
  z.object({
    state: z.literal('absent'),
    reason: z.string(),
  }),
  z.object({
    state: z.literal('observed'),
    samples: z.array(TableGrowthSampleSchema),
  }),
]);

export const StoreTelemetryEntrySchema = z.object({
  store: z.string(),
  role: z.string(),
  path: z.string(),
  read: StorageTelemetryReadSchema,
  total_bytes: z.number().nullable(),
  free_bytes: z.number().nullable(),
  free_page_ratio: z.number().nullable(),
  budget: StoreBudgetDimensionSchema,
  growth: StoreGrowthDimensionSchema,
});
export type StoreTelemetryEntry = z.infer<typeof StoreTelemetryEntrySchema>;

export const StorageTelemetryPayloadSchema = z.object({
  stores: z.array(StoreTelemetryEntrySchema),
  budget_note: z.string(),
  growth_note: z.string(),
});
export type StorageTelemetryPayload = z.infer<typeof StorageTelemetryPayloadSchema>;

/* ---- /api/storage/findings payload (storage_findings_api.rs) ---- */

export const DoctorStorageFindingKindSchema = z.enum([
  'over_budget_store',
  'orphan_store',
  'stale_branch_dbs',
  'incident_debris_present',
  'retention_backlog',
]);
export type DoctorStorageFindingKind = z.infer<typeof DoctorStorageFindingKindSchema>;

export const DoctorEvidenceStateSchema = z.enum([
  'unsupported',
  'absent',
  'stale',
  'degraded',
  'partial',
  'unknown',
  'denied',
  'healthy_complete_coverage',
]);
export type DoctorEvidenceState = z.infer<typeof DoctorEvidenceStateSchema>;

export const StorageFindingKindStatusSchema = z.object({
  kind: DoctorStorageFindingKindSchema,
  state: DoctorEvidenceStateSchema,
  required_source: z.string(),
  reason: z.string(),
});
export type StorageFindingKindStatus = z.infer<typeof StorageFindingKindStatusSchema>;

export const StorageFindingsPayloadSchema = z.object({
  kinds: z.array(StorageFindingKindStatusSchema),
  note: z.string(),
});
export type StorageFindingsPayload = z.infer<typeof StorageFindingsPayloadSchema>;
