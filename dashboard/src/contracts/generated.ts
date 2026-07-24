// Canonical wire boundary for the V2 dashboard read-model routes.
//
// SINGLE SOURCE OF TRUTH (frontend side). Every shape here is hand-matched to
// the Rust serde model in `src/dashboard/read_model.rs`
// (`DashboardEnvelopeV1<T>` plus the closed, snake_case enums). `wire.ts` and
// `index.ts` are thin re-exports of this module — there is exactly one
// wire-boundary module.
//
// TRANSITIONAL: the `dashboard/codegen/` pipeline exists to (eventually)
// generate this file from the schemars-exported JSON Schema of the same Rust
// types. Until that export is wired, this module is hand-maintained and MUST
// track `read_model.rs` exactly; the `schema_revision` literal below hard-fails
// decode on drift, and the domain-state / coverage / freshness unions are kept
// closed so the UI's exhaustive `never`-checked switches stay honest.

import { z } from 'zod';

/**
 * Exhaustiveness helper for closed-union switches. A `default` branch that
 * calls `assertNever(value)` fails to compile if a union member is left
 * unhandled, so adding a server-side variant surfaces every UI switch that
 * must handle it.
 */
export function assertNever(value: never): never {
  throw new Error(`Unhandled closed-union member: ${JSON.stringify(value)}`);
}

/** Envelope schema revision. Mirrors `DASHBOARD_SCHEMA_REVISION_V1` (u32 = 1).
 * The decoder refuses any other revision rather than silently mis-reading a
 * newer/older contract. */
export const WIRE_SCHEMA_REVISION = 1;

/** The closed 17-value dashboard domain-state union (read_model.rs
 * `DashboardDomainStateV1`, snake_case). The final `unsupported` variant is the
 * PR14 backend-gap state (the read model exists but its live producer is not
 * yet wired server-side) and is canonically emitted by the server — it is
 * distinct from `unsupported_schema` (an undecodable schema/variant). Any value
 * outside the union decodes to `unsupported_schema` rather than throwing. */
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

/** Exact resolved scope (read_model.rs `DashboardScopeV1`). A deep link never
 * falls back to a title, path, or latest version to recover scope. */
export const ScopeSchema = z.object({
  project_id: z.string().nullable(),
  storage_mode: z.string(),
  store_root: z.string(),
});
export type WireScope = z.infer<typeof ScopeSchema>;

/** Entity/graph version identity (read_model.rs `DashboardVersionV1`). Both
 * optional: no versioned graph state stays absent, never invented `0`/`latest`. */
export const VersionSchema = z.object({
  entity_version: z.string().nullable(),
  graph_version: z.string().nullable(),
});
export type WireVersion = z.infer<typeof VersionSchema>;

/** Valid time and observation time, kept separate (read_model.rs
 * `DashboardTimeV1`). Microseconds since the Unix epoch. */
export const TimeSchema = z.object({
  valid_time_micros: z.number().nullable(),
  observation_time_micros: z.number(),
});
export type WireTime = z.infer<typeof TimeSchema>;

/** Opaque monotone source watermark (read_model.rs `DashboardWatermarkV1`).
 * Compared for staleness, never parsed for internal structure. */
export const WatermarkSchema = z.object({
  source: z.string(),
  watermark: z.string(),
});
export type WireWatermark = z.infer<typeof WatermarkSchema>;

/** Authorization outcome (read_model.rs `DashboardAuthorizationV1`, internally
 * tagged on `outcome`). The local loopback dashboard only emits `authorized`
 * today; the rest are retained so the contract can express them without a
 * schema change. */
export const AuthorizationSchema = z.object({
  outcome: z.enum(['authorized', 'unauthorized', 'denied', 'redacted']),
});
export type WireAuthorization = z.infer<typeof AuthorizationSchema>;

/** Coverage statement (read_model.rs `DashboardCoverageV1`). `completeness` is
 * authoritative — the UI never derives `complete` from a `matched == eligible`
 * coincidence. An unknown denominator is `null`, never a fabricated `0`/`100%`. */
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

/** Freshness statement (read_model.rs `DashboardFreshnessV1`). `absent` (no
 * source produced anything) and `unsupported` (no source wired) are distinct
 * from `stale` (behind the watermark) and `unknown`. */
export const FreshnessSchema = z.object({
  state: z.enum(['fresh', 'stale', 'unknown', 'absent', 'unsupported']),
  observed_at_micros: z.number().nullable(),
  watermark: z.string().nullable(),
});
export type WireFreshness = z.infer<typeof FreshnessSchema>;

/** Legal-action reference kinds (read_model.rs `DashboardLegalActionKindV1`). */
export const LegalActionKindSchema = z.enum([
  'inspect',
  'expand_evidence',
  'refresh',
  'request_dry_run',
  'request_apply',
  'request_cancel',
]);
export type WireLegalActionKind = z.infer<typeof LegalActionKindSchema>;

/** A reference to one owner-supplied legal action (read_model.rs
 * `DashboardLegalActionRefV1`). `operation` names the owning application
 * operation; the dashboard never embeds argv, a path, or an inline effect. */
export const LegalActionRefSchema = z.object({
  kind: LegalActionKindSchema,
  operation: z.string(),
});
export type WireLegalActionRef = z.infer<typeof LegalActionRefSchema>;

/** The normative read-model envelope (read_model.rs `DashboardEnvelopeV1<T>`).
 * Every V2 read-model route returns exactly this shape; only `payload` varies. */
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

/* ==========================================================================
 * Route payload schemas.
 *
 * These are the `T` in `DashboardEnvelopeV1<T>` (or, for the legacy Observatory
 * routes, the raw body): hand-matched to their Rust producers rather than to
 * read_model.rs, since read_model.rs only defines the envelope + generic
 * payload slot. They live in this single boundary module alongside the
 * envelope so every wire shape has one home.
 * ======================================================================== */

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

/* ---- /api/doctor findings + remediation payloads ---- */

export const DoctorFindingFamilySchema = z.enum([
  'advisory',
  'configuration',
  'storage_runtime',
  'storage',
  'language_server',
  'semantic_index',
  'observability',
]);
export type DoctorFindingFamily = z.infer<typeof DoctorFindingFamilySchema>;

export const DoctorCoverageCompletenessSchema = z.enum(['complete', 'partial', 'unknown']);
export type DoctorCoverageCompleteness = z.infer<typeof DoctorCoverageCompletenessSchema>;

export const DoctorRemediationKindSchema = z.enum(['preview', 'action']);
export type DoctorRemediationKind = z.infer<typeof DoctorRemediationKindSchema>;

export const DoctorEvidenceRefSchema = z.object({
  family: DoctorFindingFamilySchema,
  reference: z.string(),
});
export type DoctorEvidenceRef = z.infer<typeof DoctorEvidenceRefSchema>;

export const DoctorCoverageStatementSchema = z.object({
  completeness: DoctorCoverageCompletenessSchema,
  statement: z.string(),
});
export type DoctorCoverageStatement = z.infer<typeof DoctorCoverageStatementSchema>;

export const DoctorRemediationRefSchema = z.object({
  owning_operation: z.string(),
  kind: DoctorRemediationKindSchema,
});
export type DoctorRemediationRef = z.infer<typeof DoctorRemediationRefSchema>;

export const DoctorFindingSchema = z.object({
  family: DoctorFindingFamilySchema,
  state: DoctorEvidenceStateSchema,
  evidence: z.array(DoctorEvidenceRefSchema),
  coverage: DoctorCoverageStatementSchema,
  remediation: DoctorRemediationRefSchema.nullable(),
});
export type DoctorFinding = z.infer<typeof DoctorFindingSchema>;

export const DoctorReportEntrySchema = z.object({
  finding: DoctorFindingSchema,
  storage_kind: DoctorStorageFindingKindSchema.nullable(),
});
export type DoctorReportEntry = z.infer<typeof DoctorReportEntrySchema>;

export const DoctorFamilyUnavailableReasonSchema = z.enum([
  'unwired',
  'unsupported',
  'absent',
  'denied',
  'unknown',
]);
export type DoctorFamilyUnavailableReason = z.infer<
  typeof DoctorFamilyUnavailableReasonSchema
>;

export const DoctorFamilyConsultationSchema = z.discriminatedUnion('status', [
  z.object({ status: z.literal('consulted') }),
  z.object({
    status: z.literal('unavailable'),
    reason: DoctorFamilyUnavailableReasonSchema,
  }),
]);
export type DoctorFamilyConsultation = z.infer<typeof DoctorFamilyConsultationSchema>;

export const DoctorFamilyCoverageSchema = z.object({
  family: DoctorFindingFamilySchema,
  consultation: DoctorFamilyConsultationSchema,
});
export type DoctorFamilyCoverage = z.infer<typeof DoctorFamilyCoverageSchema>;

export const DoctorReportCoverageSchema = z.object({
  families: z.array(DoctorFamilyCoverageSchema),
  completeness: DoctorCoverageCompletenessSchema,
  statement: DoctorCoverageStatementSchema,
});
export type DoctorReportCoverage = z.infer<typeof DoctorReportCoverageSchema>;

export const DoctorOwningSurfaceSchema = z.enum([
  'configuration_control_plane',
  'storage_runtime',
  'daemon_runtime',
  'host_integration',
  'semantic_index_runtime',
]);
export type DoctorOwningSurface = z.infer<typeof DoctorOwningSurfaceSchema>;

export const DoctorConfirmationRequirementSchema = z.enum(['required', 'not_required']);
export type DoctorConfirmationRequirement = z.infer<
  typeof DoctorConfirmationRequirementSchema
>;

export const DoctorRemediationDescriptorSchema = z.object({
  operation: z.string(),
  surface: DoctorOwningSurfaceSchema,
  preview_available: z.boolean(),
  action_confirmation: DoctorConfirmationRequirementSchema,
  summary: z.string(),
});
export type DoctorRemediationDescriptor = z.infer<typeof DoctorRemediationDescriptorSchema>;

export const DoctorFindingsPayloadSchema = z.object({
  family_filter: DoctorFindingFamilySchema.nullable(),
  entries: z.array(DoctorReportEntrySchema),
  report_coverage: DoctorReportCoverageSchema.nullable(),
  remediations: z.array(DoctorRemediationDescriptorSchema),
  known_families: z.array(DoctorFindingFamilySchema),
  note: z.string(),
});
export type DoctorFindingsPayload = z.infer<typeof DoctorFindingsPayloadSchema>;

/** `/api/storage/findings` — per-kind support status for the plan-38 storage
 * finding producers. This is NOT the Doctor storage-family projection: the
 * route serializes `StorageFindingsPayloadV1 { kinds, note }`
 * (src/dashboard/storage_findings_api.rs). Aliasing it to
 * `DoctorFindingsPayloadSchema` makes every response fail to parse. */
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

export const DoctorRemediationPreviewRequestSchema = z.object({
  operation: z.string(),
});
export type DoctorRemediationPreviewRequest = z.infer<
  typeof DoctorRemediationPreviewRequestSchema
>;

export const DoctorRemediationApplyRequestSchema = z.object({
  operation: z.string(),
  preview_id: z.string().nullable(),
  idempotency_key: z.string(),
  confirmed: z.boolean(),
});
export type DoctorRemediationApplyRequest = z.infer<
  typeof DoctorRemediationApplyRequestSchema
>;

export const DoctorRemediationOperationPhaseSchema = z.enum([
  'previewed',
  'running',
  'completed',
  'cancelled',
  'timed_out',
  'failed',
  'partial',
  'effect_unknown',
]);
export type DoctorRemediationOperationPhase = z.infer<
  typeof DoctorRemediationOperationPhaseSchema
>;

export const DoctorCancellationStageSchema = z.enum([
  'before_admission',
  'before_read',
  'during_read',
  'before_effect',
  'effect_in_flight',
  'reconciling',
  'after_commit',
]);

export const DoctorOperationTerminationSchema = z.enum([
  'completed',
  'cancelled',
  'timed_out',
  'failed',
  'partial',
  'effect_unknown',
]);

export const DoctorOperationReceiptSchema = z.object({
  started_at: z.number(),
  ended_at: z.number(),
  effective_deadline: z.object({ expires_at: z.number() }),
  cancellation: z
    .object({
      stage: DoctorCancellationStageSchema,
      observed_at: z.number(),
    })
    .nullable(),
  budget: z.object({
    units_consumed: z.number(),
    bytes_consumed: z.number(),
    elapsed_micros: z.number(),
  }),
  termination: DoctorOperationTerminationSchema,
});
export type DoctorOperationReceipt = z.infer<typeof DoctorOperationReceiptSchema>;

export const DoctorProjectAuthorityScopeSchema = z.object({
  scope_kind: z.literal('project'),
  project_id: z.string(),
  repository_id: z.string(),
  worktree_id: z.string(),
  reference: z.string().nullable(),
  scope_digest: z.string(),
});

export const DoctorProfileAuthorityScopeSchema = z.object({
  scope_kind: z.literal('profile'),
  brain_id: z.string(),
  profile_id: z.string(),
  profile_sessions_binding_digest: z.string(),
  scope_digest: z.string(),
});

export const DoctorRemediationAuthorityScopeSchema = z.discriminatedUnion('scope_kind', [
  DoctorProjectAuthorityScopeSchema,
  DoctorProfileAuthorityScopeSchema,
]);
export type DoctorRemediationAuthorityScope = z.infer<
  typeof DoctorRemediationAuthorityScopeSchema
>;

const DoctorEffectReceiptFields = {
  operation: z.string(),
  request_id: z.string(),
  actor: z.string(),
  effect_class: z.enum([
    'read',
    'preview',
    'source_edit',
    'git_index_stage',
    'git_index_unstage',
    'git_index_commit',
    'configuration_write',
    'administrative',
  ]),
  idempotency_key: z.string(),
  input_digest: z.string(),
  expected_state: z.string(),
  policy_digest: z.string(),
  configuration_digest: z.string(),
  catalog_digest: z.string(),
  privacy_digest: z.string(),
  outcome: DoctorOperationTerminationSchema,
  committed_state: z.string().nullable(),
  external_proof: z.string().nullable(),
};

export const DoctorEffectReceiptSchema = z.discriminatedUnion('scope_kind', [
  z.object({
    scope_kind: z.literal('project'),
    ...DoctorEffectReceiptFields,
    scope: DoctorProjectAuthorityScopeSchema.omit({ scope_kind: true }),
  }),
  z.object({
    scope_kind: z.literal('profile'),
    ...DoctorEffectReceiptFields,
    scope: DoctorProfileAuthorityScopeSchema.omit({ scope_kind: true }),
  }),
]);
export type DoctorEffectReceipt = z.infer<typeof DoctorEffectReceiptSchema>;

export const DoctorRemediationOperationSchema = z.object({
  operation_id: z.string(),
  owning_operation: z.string(),
  authority_scope: DoctorRemediationAuthorityScopeSchema,
  phase: DoctorRemediationOperationPhaseSchema,
  preview_id: z.string().nullable(),
  idempotency_key: z.string().nullable(),
  execution: DoctorOperationReceiptSchema.nullable(),
  effect_receipt: DoctorEffectReceiptSchema.nullable(),
});
export type DoctorRemediationOperation = z.infer<
  typeof DoctorRemediationOperationSchema
>;

export const DoctorRemediationDispatchErrorSchema = z.enum([
  'unsupported',
  'denied',
  'invalid_reference',
  'confirmation_required',
  'owner_unavailable',
]);
export type DoctorRemediationDispatchError = z.infer<
  typeof DoctorRemediationDispatchErrorSchema
>;

export const DoctorRemediationPayloadSchema = z.discriminatedUnion('status', [
  z.object({
    status: z.literal('operation'),
    operation: DoctorRemediationOperationSchema,
  }),
  z.object({
    status: z.literal('unavailable'),
    reason: DoctorRemediationDispatchErrorSchema,
  }),
]);
export type DoctorRemediationPayload = z.infer<typeof DoctorRemediationPayloadSchema>;
