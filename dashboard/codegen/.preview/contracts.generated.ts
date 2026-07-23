// GENERATED PREVIEW by dashboard/codegen — not the live wire boundary.
// Source: dashboard/codegen/schemas/*.schema.json (kept in lockstep with
// src/dashboard/read_model.rs). The live boundary at src/contracts/generated.ts
// is hand-maintained until the real schemars export replaces this harness.
// Deterministic output (stable ordering, no timestamps).

import { z } from "zod";

/**
 * Exhaustiveness helper for discriminated-union switches. A `default`
 * branch that calls `assertNever(value)` fails to compile if a union
 * member is left unhandled.
 */
export function assertNever(value: never): never {
  throw new Error(`Unhandled discriminated union member: ${JSON.stringify(value)}`);
}

export const SCHEMA_REVISION = "read-model-v1" as const;

/** Authorization outcome (internally tagged on `outcome`). Only `authorized` is emitted by the local loopback dashboard today. */
export interface Authorization {
  outcome: "authorized" | "denied" | "redacted" | "unauthorized";
}

export const AuthorizationSchema: z.ZodType<Authorization> = z.object({
  outcome: z.enum(["authorized", "denied", "redacted", "unauthorized"]),
});

/** Coverage statement. `completeness` is authoritative; an unknown denominator is null, never a fabricated 0/100%. */
export interface Coverage {
  completeness: "complete" | "partial" | "unknown" | "unsupported";
  denominator: number | null;
  eligible: number | null;
  examined: number | null;
  excluded: number | null;
  matched: number | null;
  omission_reasons: Array<string>;
  omitted: number | null;
  unit: string | null;
  unknown: number | null;
}

export const CoverageSchema: z.ZodType<Coverage> = z.object({
  completeness: z.enum(["complete", "partial", "unknown", "unsupported"]),
  denominator: z.number().int().nullable(),
  eligible: z.number().int().nullable(),
  examined: z.number().int().nullable(),
  excluded: z.number().int().nullable(),
  matched: z.number().int().nullable(),
  omission_reasons: z.array(z.string()),
  omitted: z.number().int().nullable(),
  unit: z.string().nullable(),
  unknown: z.number().int().nullable(),
});

/** The closed 17-value domain-state union (snake_case). `unsupported` (live producer not yet wired server-side) is canonically emitted by the server and is distinct from `unsupported_schema` (an undecodable schema/variant). */
export type DashboardDomainState = "cancelled" | "complete_zero_findings" | "conflicting" | "denied" | "error" | "loading" | "locked" | "offline" | "partial" | "ready" | "redacted" | "stale" | "timed_out" | "unauthorized" | "unknown" | "unsupported" | "unsupported_schema";

export const DashboardDomainStateSchema: z.ZodType<DashboardDomainState> = z.enum(["cancelled", "complete_zero_findings", "conflicting", "denied", "error", "loading", "locked", "offline", "partial", "ready", "redacted", "stale", "timed_out", "unauthorized", "unknown", "unsupported", "unsupported_schema"]);

/** DashboardEnvelopeV1<TPayload>. Every V2 read-model route returns exactly this shape; only `payload` varies. Carries schema revision, exact scope, entity/graph version, valid+observation time, optional source watermark, authorization, coverage, freshness, domain state, legal action references, and payload. */
export interface DashboardEnvelope<TPayload> {
  authorization: Authorization;
  coverage: Coverage;
  domain_state: DashboardDomainState;
  freshness: Freshness;
  legal_actions: Array<LegalActionRef>;
  payload: TPayload;
  schema_revision: number;
  scope: Scope;
  source_watermark: Watermark | null;
  time: Time;
  version: Version;
}

export function DashboardEnvelopeSchema<TPayload>(
  payloadSchema: z.ZodType<TPayload>,
): z.ZodType<DashboardEnvelope<TPayload>> {
  return z.object({
    authorization: z.lazy(() => AuthorizationSchema),
    coverage: z.lazy(() => CoverageSchema),
    domain_state: z.lazy(() => DashboardDomainStateSchema),
    freshness: z.lazy(() => FreshnessSchema),
    legal_actions: z.array(z.lazy(() => LegalActionRefSchema)),
    payload: payloadSchema,
    schema_revision: z.number().int(),
    scope: z.lazy(() => ScopeSchema),
    source_watermark: z.union([z.lazy(() => WatermarkSchema), z.null()]),
    time: z.lazy(() => TimeSchema),
    version: z.lazy(() => VersionSchema),
  }) as unknown as z.ZodType<DashboardEnvelope<TPayload>>;
}

/** Representative domain payload (the `T` in DashboardEnvelope<T>). Severity and evidence quality are separate token axes, never one scale. */
export interface FindingPayload {
  evidence_quality: "corroborated" | "unverified" | "verified" | "weak";
  finding_id: string;
  severity: "critical" | "high" | "info" | "low" | "medium";
  summary: string;
  title: string;
}

export const FindingPayloadSchema: z.ZodType<FindingPayload> = z.object({
  evidence_quality: z.enum(["corroborated", "unverified", "verified", "weak"]),
  finding_id: z.string(),
  severity: z.enum(["critical", "high", "info", "low", "medium"]),
  summary: z.string(),
  title: z.string(),
});

/** Freshness statement. `absent` (no source produced anything) and `unsupported` (no source wired) are distinct from `stale` and `unknown`. */
export interface Freshness {
  observed_at_micros: number | null;
  state: "absent" | "fresh" | "stale" | "unknown" | "unsupported";
  watermark: string | null;
}

export const FreshnessSchema: z.ZodType<Freshness> = z.object({
  observed_at_micros: z.number().int().nullable(),
  state: z.enum(["absent", "fresh", "stale", "unknown", "unsupported"]),
  watermark: z.string().nullable(),
});

export type LegalActionKind = "expand_evidence" | "inspect" | "refresh" | "request_apply" | "request_cancel" | "request_dry_run";

export const LegalActionKindSchema: z.ZodType<LegalActionKind> = z.enum(["expand_evidence", "inspect", "refresh", "request_apply", "request_cancel", "request_dry_run"]);

/** Reference to one owner-supplied legal action. No command payload; `operation` names the owning application operation. */
export interface LegalActionRef {
  kind: LegalActionKind;
  operation: string;
}

export const LegalActionRefSchema: z.ZodType<LegalActionRef> = z.object({
  kind: z.lazy(() => LegalActionKindSchema),
  operation: z.string(),
});

/** Exact resolved scope. Never falls back to a title, path, or latest version. */
export interface Scope {
  project_id: string | null;
  storage_mode: string;
  store_root: string;
}

export const ScopeSchema: z.ZodType<Scope> = z.object({
  project_id: z.string().nullable(),
  storage_mode: z.string(),
  store_root: z.string(),
});

/** Valid time and observation time, kept separate. Microseconds since the Unix epoch. */
export interface Time {
  observation_time_micros: number;
  valid_time_micros: number | null;
}

export const TimeSchema: z.ZodType<Time> = z.object({
  observation_time_micros: z.number().int(),
  valid_time_micros: z.number().int().nullable(),
});

/** Entity/graph version identity. Both optional; absent rather than invented 0/latest. */
export interface Version {
  entity_version: string | null;
  graph_version: string | null;
}

export const VersionSchema: z.ZodType<Version> = z.object({
  entity_version: z.string().nullable(),
  graph_version: z.string().nullable(),
});

/** Opaque monotone source watermark. Compared for staleness, never parsed. */
export interface Watermark {
  source: string;
  watermark: string;
}

export const WatermarkSchema: z.ZodType<Watermark> = z.object({
  source: z.string(),
  watermark: z.string(),
});
