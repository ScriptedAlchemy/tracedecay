/**
 * The Observatory's evidence model: eleven independent authorities, each
 * reduced to one typed summary the panel grid, the observation timeline, and
 * the evidence inspector all read.
 *
 * Every function here is pure and total. It never averages two authorities,
 * never promotes a weaker evidence rung to a stronger one, and never turns a
 * missing read into a zero, a green lamp, or the word "nominal". A summary's
 * `state` is derived from what the daemon said — its envelope domain state,
 * its coverage statement, and a handful of payload facts that name an
 * in-flight build — and the daemon's own word travels beside it as
 * `stateDetail` whenever the grade is coarser than the wire.
 */
import type {
  AnalyticsDiagnosticsPayloadV1,
  AnalyticsHintsPayloadV1,
  CodeIndexFreshnessPayloadV1,
  DashboardCoverageCompletenessV1,
  DashboardDomainStateV1,
  DashboardEnvelopeV1,
  DoctorFindingsPayloadV1,
  ExecutionTopologyMetricsV1,
  ObservatoryReadModelV1,
  StorageFindingsPayloadV1,
  StorageTelemetryPayloadV1,
} from '../../contracts/generated.ts';
import { assertNever } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult } from '../work/workApi.ts';
import { adoptionCoverageBands, coverageTotals } from './adoptionCoverage.ts';
import { analyticsModeReading } from './analyticsControls.ts';
import { budgetCoverage, performanceBudgetBands } from './performanceBudgets.ts';
import { retrievalCoverage, retrievalQualityBands } from './retrievalQuality.ts';

/** The authorities the overview reports, in the order the grid draws them. The
 * timeline (`observations`) is itself a source: the canonical read model owns
 * the shared horizon, so it is inspectable like everything else. */
export const EVIDENCE_SOURCES = [
  'observations',
  'doctor',
  'adoption',
  'retrieval',
  'pipeline',
  'hooks',
  'budgets',
  'topology',
  'analytics',
  'telemetry',
  'findings',
] as const;

export type EvidenceSourceId = (typeof EVIDENCE_SOURCES)[number];

export function isEvidenceSourceId(value: string | null | undefined): value is EvidenceSourceId {
  return EVIDENCE_SOURCES.some((id) => id === value);
}

/**
 * The typed evidence grades a panel can wear. Coarser than the nineteen-state
 * domain taxonomy on purpose — the overview is read at a glance — but never
 * lossy: the daemon's exact word rides in `stateDetail` when the two differ.
 *
 *   measured     served, complete, current
 *   empty        served and complete with nothing in it — a measured zero
 *   partial      served with less than everything the authority knows exists
 *   stale        served earlier; the source has moved past it
 *   building     a real build or convergence is in flight
 *   loading      the request is in flight
 *   restricted   the identity may not see all of it (locked, redacted, unsupported)
 *   denied       the daemon refused the read
 *   failed       the read ran and did not produce a result
 *   unavailable  nothing was reached, or the authority said it cannot answer
 */
export type EvidenceState =
  | 'measured'
  | 'empty'
  | 'partial'
  | 'stale'
  | 'building'
  | 'loading'
  | 'restricted'
  | 'denied'
  | 'failed'
  | 'unavailable';

export const EVIDENCE_STATES: readonly EvidenceState[] = [
  'measured',
  'empty',
  'partial',
  'stale',
  'building',
  'loading',
  'restricted',
  'denied',
  'failed',
  'unavailable',
];

export interface EvidenceCoverage {
  completeness: DashboardCoverageCompletenessV1;
  examined: number | null;
  denominator: number | null;
  unit: string | null;
}

export interface EvidenceFreshness {
  state: string;
  observedAtMicros: number | null;
  watermark: string | null;
}

export interface EvidenceScope {
  projectId: string | null;
  storageMode: string;
  storeRoot: string;
}

export interface DeclaredAction {
  kind: string;
  operation: string;
}

export interface EvidenceSummary {
  id: EvidenceSourceId;
  /** Engraved legend on the panel and title of the inspector. */
  title: string;
  /** The production route this summary was read from. */
  route: string;
  /** One sentence naming the owning authority. */
  authority: string;
  state: EvidenceState;
  /** The daemon's own word or reason when `state` is coarser than the wire. */
  stateDetail: string | null;
  /** The authority's coverage statement, or `null` when it published none. */
  coverage: EvidenceCoverage | null;
  /** When the authority observed what it reported; `null` when it said nothing. */
  observedAtMicros: number | null;
  freshness: EvidenceFreshness | null;
  scope: EvidenceScope | null;
  authorization: string | null;
  watermark: string | null;
  /** What the reading is about, as a count sentence. `null` when the read
   * produced nothing to count. */
  affected: string | null;
  /** The payload's own note, verbatim. */
  note: string | null;
  /** The legal actions the envelope declared, verbatim. */
  declaredActions: readonly DeclaredAction[];
  /** The server's refresh operation, when it offered one. */
  refreshOperation: string | null;
  /** When this browser last received a successful read, in epoch ms; `0` when
   * it never has. Client-side provenance, labelled as such. */
  lastReadMs: number;
}

/** What a summary is built from: one read, as React Query holds it. */
export interface EvidenceRead<T> {
  pending: boolean;
  result: EnvelopeResult<T> | undefined;
  /** React Query's `dataUpdatedAt`: when the last successful result landed. */
  updatedAtMs: number;
}

interface SourceIdentity {
  id: EvidenceSourceId;
  title: string;
  route: string;
  authority: string;
}

export const SOURCE_IDENTITY: Record<EvidenceSourceId, SourceIdentity> = {
  observations: {
    id: 'observations',
    title: 'Canonical observations',
    route: '/api/observatory',
    authority:
      'Plan 26 canonical read model — the horizon, watermark, and metrics the CLI and MCP serve',
  },
  doctor: {
    id: 'doctor',
    title: 'Doctor inspection',
    route: '/api/doctor/findings',
    authority: 'canonical Doctor report — finding families, evidence states, report coverage',
  },
  adoption: {
    id: 'adoption',
    title: 'Adoption coverage',
    route: '/api/observatory',
    authority: 'canonical adoption dimensions, with record counts from analytics diagnostics',
  },
  retrieval: {
    id: 'retrieval',
    title: 'Retrieval quality',
    route: '/api/observatory',
    authority: 'canonical retrieval dimensions, with record counts from analytics diagnostics',
  },
  pipeline: {
    id: 'pipeline',
    title: 'Code-index pipeline',
    route: '/api/code-index/freshness',
    authority: 'daemon scheduler state — sealed generations, live build progress, clone index',
  },
  hooks: {
    id: 'hooks',
    title: 'Hook hints · rejections',
    route: '/api/plugins/analytics/hints',
    authority: 'typed hint summary over durable analytics events; rejected arguments from the canonical read model',
  },
  budgets: {
    id: 'budgets',
    title: 'Performance budgets',
    route: '/api/observatory',
    authority: 'canonical performance budgets and the comparison disposition',
  },
  topology: {
    id: 'topology',
    title: 'Execution topology',
    route: '/api/work/topology-metrics',
    authority: 'Work-owned execution-topology projection — measurement cells with typed omissions',
  },
  analytics: {
    id: 'analytics',
    title: 'Analytics controls',
    route: '/api/observatory',
    authority: 'canonical analytics mode, retention lifecycle, egress and upload settings',
  },
  telemetry: {
    id: 'telemetry',
    title: 'Storage telemetry',
    route: '/api/storage/telemetry',
    authority: 'store telemetry — measured sizes, budgets, growth, per-table growth',
  },
  findings: {
    id: 'findings',
    title: 'Storage findings',
    route: '/api/storage/findings',
    authority: 'storage-family projection of the admitted canonical Doctor report',
  },
};

/** The grade a daemon domain state reads as. Exhaustive, so a new wire state
 * fails to build here rather than landing in a healthy-looking bucket. */
export function evidenceStateOf(state: DashboardDomainStateV1 | DomainStateKind): EvidenceState {
  switch (state) {
    case 'ready':
      return 'measured';
    case 'complete_zero_findings':
      return 'empty';
    case 'partial':
    case 'rate_limited':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'loading':
      return 'loading';
    case 'locked':
    case 'redacted':
    case 'unsupported':
    case 'unsupported_schema':
      return 'restricted';
    case 'denied':
    case 'unauthorized':
      return 'denied';
    case 'error':
    case 'cancelled':
    case 'timed_out':
    case 'conflicting':
      return 'failed';
    case 'offline':
    case 'unknown':
    case 'unavailable':
      return 'unavailable';
    default:
      return assertNever(state);
  }
}

/** The word a grade is printed as. Uppercase happens in CSS. */
export function evidenceStateLabel(state: EvidenceState): string {
  switch (state) {
    case 'measured':
      return 'measured';
    case 'empty':
      return 'measured · empty';
    case 'partial':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'building':
      return 'building';
    case 'loading':
      return 'loading';
    case 'restricted':
      return 'restricted';
    case 'denied':
      return 'denied';
    case 'failed':
      return 'failed';
    case 'unavailable':
      return 'unavailable';
    default:
      return assertNever(state);
  }
}

/** Lamp and ink per grade. Spelled out literally because Tailwind scans source
 * text for utilities; a computed class would never be built. Colour is never
 * the only carrier — the word is always printed beside the lamp. */
export function evidenceTone(state: EvidenceState): { lamp: string; ink: string } {
  switch (state) {
    case 'measured':
      return { lamp: 'bg-state-ready', ink: 'text-state-ready' };
    case 'empty':
      return { lamp: 'bg-state-complete-zero', ink: 'text-state-complete-zero' };
    case 'partial':
      return { lamp: 'bg-state-partial', ink: 'text-state-partial' };
    case 'stale':
      return { lamp: 'bg-state-stale', ink: 'text-state-stale' };
    case 'building':
      return { lamp: 'bg-alert', ink: 'text-alert' };
    case 'loading':
      return { lamp: 'bg-state-loading', ink: 'text-state-loading' };
    case 'restricted':
      return { lamp: 'bg-state-locked', ink: 'text-state-locked' };
    case 'denied':
      return { lamp: 'bg-state-denied', ink: 'text-state-denied' };
    case 'failed':
      return { lamp: 'bg-state-error', ink: 'text-state-error' };
    case 'unavailable':
      return { lamp: 'bg-state-unknown', ink: 'text-state-unknown' };
    default:
      return assertNever(state);
  }
}

/** A percentage only when both figures are real and the denominator is
 * positive. An unknown denominator never becomes `0%` or `100%`. */
export function coveragePercent(coverage: EvidenceCoverage | null): number | null {
  if (!coverage) return null;
  const { examined, denominator } = coverage;
  if (examined == null || denominator == null || denominator <= 0) return null;
  return Math.max(0, Math.min(100, Math.round((examined / denominator) * 100)));
}

export function coverageSentence(coverage: EvidenceCoverage | null): string {
  if (!coverage) return 'no coverage statement published';
  const unit = coverage.unit ?? 'units';
  if (coverage.examined != null && coverage.denominator != null) {
    return `${coverage.examined.toLocaleString()} of ${coverage.denominator.toLocaleString()} ${unit} · ${coverage.completeness}`;
  }
  if (coverage.examined != null) {
    return `${coverage.examined.toLocaleString()} ${unit} examined · denominator unknown · ${coverage.completeness}`;
  }
  return `${coverage.completeness} · counts not published`;
}

/**
 * Everything an envelope says about itself, lifted into the summary shape.
 * The blocked outcomes each become their own grade with the daemon's reason;
 * a decoded envelope contributes its truth header and then hands the payload
 * to `refine`, which may only narrow the grade with facts the payload carries
 * (an in-flight build, a served-empty body) — never widen it toward healthy.
 */
function envelopeSummary<T>(
  identity: SourceIdentity,
  read: EvidenceRead<T>,
  refine: (payload: T, envelope: DashboardEnvelopeV1<T>) => Partial<
    Pick<EvidenceSummary, 'state' | 'stateDetail' | 'coverage' | 'observedAtMicros' | 'affected' | 'note' | 'watermark'>
  >,
): EvidenceSummary {
  const base: EvidenceSummary = {
    ...identity,
    state: 'unavailable',
    stateDetail: null,
    coverage: null,
    observedAtMicros: null,
    freshness: null,
    scope: null,
    authorization: null,
    watermark: null,
    affected: null,
    note: null,
    declaredActions: [],
    refreshOperation: null,
    lastReadMs: read.updatedAtMs,
  };
  if (read.pending) {
    return { ...base, state: 'loading', stateDetail: 'request in flight' };
  }
  const result = read.result;
  if (result === undefined) {
    return { ...base, state: 'unavailable', stateDetail: 'no response recorded' };
  }
  if (result.outcome === 'transport') {
    return {
      ...base,
      state: evidenceStateOf(result.state),
      stateDetail: result.detail ?? result.state.replaceAll('_', ' '),
    };
  }
  const envelope = result.envelope;
  const grade = evidenceStateOf(envelope.domain_state);
  const fromEnvelope: EvidenceSummary = {
    ...base,
    state: grade,
    stateDetail: gradeDetail(grade, envelope.domain_state),
    coverage: {
      completeness: envelope.coverage.completeness,
      examined: envelope.coverage.examined,
      denominator: envelope.coverage.denominator ?? envelope.coverage.eligible,
      unit: envelope.coverage.unit,
    },
    observedAtMicros:
      envelope.freshness.observed_at_micros ?? envelope.time.observation_time_micros,
    freshness: {
      state: envelope.freshness.state,
      observedAtMicros: envelope.freshness.observed_at_micros,
      watermark: envelope.freshness.watermark,
    },
    scope: {
      projectId: envelope.scope.project_id,
      storageMode: envelope.scope.storage_mode,
      storeRoot: envelope.scope.store_root,
    },
    authorization: envelope.authorization.outcome,
    watermark:
      envelope.source_watermark == null
        ? null
        : `${envelope.source_watermark.source} · ${envelope.source_watermark.watermark}`,
    declaredActions: envelope.legal_actions.map((action) => ({
      kind: action.kind,
      operation: action.operation,
    })),
    refreshOperation:
      envelope.legal_actions.find((action) => action.kind === 'refresh')?.operation ?? null,
  };
  return { ...fromEnvelope, ...refine(envelope.payload, envelope) };
}

/** The daemon's word rides beside the grade only where the grade is coarser
 * than the wire — a served grade already says everything its wire state does,
 * while a refusal or absence has several causes worth naming. */
function gradeDetail(grade: EvidenceState, wire: DashboardDomainStateV1): string | null {
  switch (grade) {
    case 'measured':
    case 'empty':
    case 'partial':
    case 'stale':
    case 'loading':
    case 'building':
      return null;
    case 'restricted':
    case 'denied':
    case 'failed':
    case 'unavailable':
      return wire.replaceAll('_', ' ');
    default:
      return assertNever(grade);
  }
}

// ---------------------------------------------------------------------------
// Per-source summaries
// ---------------------------------------------------------------------------

export function observationsSummary(read: EvidenceRead<ObservatoryReadModelV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.observations, read, (model) => ({
    observedAtMicros: model.observed_at_micros,
    watermark: model.watermark,
    affected: `${model.metrics.length.toLocaleString()} metrics · ${
      model.metrics.filter((metric) => metric.value != null).length
    } carry a figure`,
    note: model.current ? null : `not current · watermark ${model.watermark}`,
  }));
}

export function doctorSummary(read: EvidenceRead<DoctorFindingsPayloadV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.doctor, read, (payload, envelope) => {
    const problems = payload.entries.filter(
      (entry) => entry.finding.state === 'degraded' || entry.finding.state === 'stale',
    ).length;
    const unavailableFamilies =
      payload.report_coverage?.families.filter(
        (family) => family.consultation.status === 'unavailable',
      ).length ?? 0;
    const consulted =
      payload.report_coverage?.families.filter(
        (family) => family.consultation.status === 'consulted',
      ).length ?? null;
    const families = payload.report_coverage?.families.length ?? null;
    const grade = evidenceStateOf(envelope.domain_state);
    return {
      state: grade === 'measured' && payload.entries.length === 0 ? 'empty' : grade,
      coverage:
        payload.report_coverage == null
          ? null
          : {
              completeness: payload.report_coverage.completeness,
              examined: consulted,
              denominator: families,
              unit: 'families',
            },
      affected: `${payload.entries.length.toLocaleString()} findings · ${problems} problem · ${unavailableFamilies} families unavailable · ${payload.schema_convergences.length} schema convergences`,
      note: payload.note,
    };
  });
}

/**
 * A panel-specific coverage count under the envelope's own completeness word.
 * The count is the panel's — how many of its required dimensions the read
 * carried — but the completeness axis stays the daemon's: a full set of
 * figures never upgrades a `partial` envelope to `complete`, and a short set
 * can only narrow it.
 */
function dimensionCoverage(
  envelope: DashboardEnvelopeV1<unknown>,
  totals: { measured: number; required: number },
  unit: string,
): EvidenceCoverage {
  return {
    completeness:
      totals.measured < totals.required && envelope.coverage.completeness === 'complete'
        ? 'partial'
        : envelope.coverage.completeness,
    examined: totals.measured,
    denominator: totals.required,
    unit,
  };
}

export function adoptionSummary(read: EvidenceRead<ObservatoryReadModelV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.adoption, read, (model, envelope) => {
    const totals = coverageTotals(adoptionCoverageBands(model));
    return {
      observedAtMicros: model.observed_at_micros,
      watermark: model.watermark,
      coverage: dimensionCoverage(envelope, totals, 'adoption dimensions'),
      affected: `${totals.measured} of ${totals.required} dimensions carry a figure · ${totals.unprojected} unpublished`,
    };
  });
}

export function retrievalSummary(read: EvidenceRead<ObservatoryReadModelV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.retrieval, read, (model, envelope) => {
    const totals = retrievalCoverage(retrievalQualityBands(model));
    return {
      observedAtMicros: model.observed_at_micros,
      watermark: model.watermark,
      coverage: dimensionCoverage(envelope, totals, 'retrieval dimensions'),
      affected: `${totals.measured} of ${totals.required} dimensions carry a figure`,
    };
  });
}

export function budgetsSummary(read: EvidenceRead<ObservatoryReadModelV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.budgets, read, (model, envelope) => {
    const totals = budgetCoverage(performanceBudgetBands(model));
    return {
      observedAtMicros: model.observed_at_micros,
      watermark: model.watermark,
      coverage: dimensionCoverage(envelope, totals, 'budget dimensions'),
      affected: `${totals.measured} of ${totals.required} budget dimensions carry a figure · comparison ${model.comparison.disposition.replaceAll('_', ' ')}`,
    };
  });
}

export function analyticsSummary(read: EvidenceRead<ObservatoryReadModelV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.analytics, read, (model) => {
    const mode = analyticsModeReading(model.analytics_mode);
    return {
      observedAtMicros: model.observed_at_micros,
      watermark: model.watermark,
      affected: `collection mode ${mode.label}${mode.reason ? ` · ${mode.reason}` : ''}`,
    };
  });
}

export function pipelineSummary(read: EvidenceRead<CodeIndexFreshnessPayloadV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.pipeline, read, (payload, envelope) => {
    const worktrees = payload.worktrees;
    const building = worktrees.filter(
      (worktree) =>
        (worktree.progress != null && worktree.progress.phase !== 'ready') ||
        worktree.rebuild_in_flight ||
        worktree.clone_index?.state === 'backfilling',
    ).length;
    const stale = worktrees.filter((worktree) => worktree.staleness_state === 'stale').length;
    const blocked = worktrees.filter((worktree) => worktree.progress?.blocked_reason != null).length;
    const grade = evidenceStateOf(envelope.domain_state);
    // Only a served, complete read may say the scope is empty; a build in
    // flight narrows a served grade to `building`; refusals stay refusals.
    const state: EvidenceState =
      grade === 'denied' || grade === 'failed' || grade === 'restricted' || grade === 'unavailable'
        ? grade
        : worktrees.length === 0
          ? grade === 'measured'
            ? 'empty'
            : grade
          : building > 0
            ? 'building'
            : grade;
    return {
      state,
      stateDetail:
        worktrees.length === 0
          ? 'no mounted code-index worktree'
          : building > 0
            ? `${building} of ${worktrees.length} worktrees building${blocked > 0 ? ` · ${blocked} blocked` : ''}`
            : null,
      affected: `${worktrees.length.toLocaleString()} worktrees · ${building} building · ${stale} stale · ${blocked} blocked`,
      note: payload.note,
    };
  });
}

export function hooksSummary(
  read: EvidenceRead<AnalyticsHintsPayloadV1>,
): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.hooks, read, (payload, envelope) => {
    if (payload == null) {
      return { state: 'unavailable', stateDetail: 'the daemon sent no hint payload' };
    }
    if (!payload.available) {
      return {
        state: 'unavailable',
        stateDetail: payload.error ?? 'the hint analytics source is unavailable',
        note: `source: ${payload.source}`,
      };
    }
    const emitted = payload.by_category.reduce((sum, category) => sum + category.emitted, 0);
    const grade = evidenceStateOf(envelope.domain_state);
    return {
      state: grade === 'measured' && payload.by_category.length === 0 ? 'empty' : grade,
      stateDetail: payload.error,
      affected: `${payload.by_category.length.toLocaleString()} categories · ${emitted.toLocaleString()} hints emitted`,
      note: `source: ${payload.source}`,
    };
  });
}

export function telemetrySummary(read: EvidenceRead<StorageTelemetryPayloadV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.telemetry, read, (payload, envelope) => {
    const stores = payload.stores;
    const sampled = stores.filter((store) => store.read.kind === 'observed').length;
    const sizeOnly = stores.filter((store) => store.read.kind === 'observed_bytes').length;
    const unread = stores.length - sampled - sizeOnly;
    const over = stores.filter(
      (store) =>
        store.budget.state === 'evaluated' && store.budget.evaluation.state === 'over_budget',
    ).length;
    const grade = evidenceStateOf(envelope.domain_state);
    return {
      state: grade === 'measured' && stores.length === 0 ? 'empty' : grade,
      stateDetail: stores.length === 0 ? 'telemetry payload contained no stores' : null,
      affected: `${stores.length.toLocaleString()} stores · ${sampled} sampled · ${sizeOnly} size only · ${unread} unread · ${over} over budget`,
      note: payload.budget_note,
    };
  });
}

export function findingsSummary(read: EvidenceRead<StorageFindingsPayloadV1>): EvidenceSummary {
  return envelopeSummary(SOURCE_IDENTITY.findings, read, (payload, envelope) => {
    const problems = payload.entries.filter(
      (entry) => entry.finding.state === 'degraded' || entry.finding.state === 'stale',
    ).length;
    const real = payload.kind_statuses.filter((status) => status.state === 'real').length;
    const grade = evidenceStateOf(envelope.domain_state);
    return {
      state: grade === 'measured' && payload.entries.length === 0 ? 'empty' : grade,
      coverage: dimensionCoverage(
        envelope,
        { measured: real, required: payload.kind_statuses.length },
        'producers real',
      ),
      affected: `${payload.entries.length.toLocaleString()} findings · ${problems} problem · ${real} of ${payload.kind_statuses.length} producers real`,
      note: payload.note,
    };
  });
}

/** The topology projection arrives through the Work command ladder, not the
 * envelope ladder, so its truth header is the model's own. */
export function topologySummary(read: {
  pending: boolean;
  result: WorkResult<ExecutionTopologyMetricsV1> | undefined;
  updatedAtMs: number;
}): EvidenceSummary {
  const identity = SOURCE_IDENTITY.topology;
  const base: EvidenceSummary = {
    ...identity,
    state: 'unavailable',
    stateDetail: null,
    coverage: null,
    observedAtMicros: null,
    freshness: null,
    scope: null,
    authorization: null,
    watermark: null,
    affected: null,
    note: null,
    declaredActions: [],
    refreshOperation: null,
    lastReadMs: read.updatedAtMs,
  };
  if (read.pending) return { ...base, state: 'loading', stateDetail: 'request in flight' };
  if (read.result === undefined) {
    return { ...base, stateDetail: 'no response recorded' };
  }
  if (read.result.outcome === 'refused') {
    return {
      ...base,
      state: evidenceStateOf(read.result.state),
      stateDetail: read.result.detail,
    };
  }
  const model = read.result.value;
  const measured = model.measurements.filter((cell) => cell.value.value != null).length;
  return {
    ...base,
    state: topologyGrade(model.coverage.state),
    stateDetail: `${model.coverage.state} family coverage${model.current ? '' : ' · not current'}`,
    coverage: {
      // The projection's own coverage state is the completeness axis; the
      // cell count only narrows it.
      completeness:
        model.coverage.state === 'unknown'
          ? 'unknown'
          : model.coverage.state !== 'known' || measured < model.measurements.length
            ? 'partial'
            : 'complete',
      examined: measured,
      denominator: model.measurements.length,
      unit: 'measurement cells',
    },
    observedAtMicros: model.observed_at_micros,
    freshness: {
      state: model.current ? 'current' : 'not current',
      observedAtMicros: model.observed_at_micros,
      watermark: model.watermark,
    },
    scope: null,
    authorization: null,
    watermark: model.watermark,
    affected: `${model.measurements.length.toLocaleString()} measurement cells · ${measured} carry a figure · ${model.drill_anchors.length} drill anchors`,
    note: `authorized scope ${model.authorized_scope_ref}`,
  };
}

function topologyGrade(state: ExecutionTopologyMetricsV1['coverage']['state']): EvidenceState {
  switch (state) {
    case 'known':
      return 'measured';
    case 'partial':
    case 'sampled':
    case 'capped':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'unknown':
      return 'unavailable';
    default:
      return assertNever(state);
  }
}

/** The diagnostics record-count window is a second source the adoption and
 * retrieval panels cite beside the canonical dimensions. It is summarised
 * separately so the panel can say two things without averaging them. */
export function diagnosticsWindowWord(
  read: EvidenceRead<AnalyticsDiagnosticsPayloadV1>,
): { state: EvidenceState; detail: string } {
  if (read.pending) return { state: 'loading', detail: 'record counts in flight' };
  if (read.result === undefined) return { state: 'unavailable', detail: 'no record-count response' };
  if (read.result.outcome === 'transport') {
    return {
      state: evidenceStateOf(read.result.state),
      detail: read.result.detail ?? 'record counts could not be read',
    };
  }
  const envelope = read.result.envelope;
  if (!envelope.payload.available) {
    return { state: 'unavailable', detail: `record counts unavailable · ${envelope.payload.source}` };
  }
  return {
    state: evidenceStateOf(envelope.domain_state),
    detail: `${envelope.coverage.completeness} window · ${envelope.payload.event_count.toLocaleString()} events · ${envelope.payload.source}`,
  };
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

export interface TimelineMark {
  id: EvidenceSourceId;
  title: string;
  state: EvidenceState;
  observedAtMicros: number;
  /** 0–1 along the rail. */
  position: number;
}

/** Marks too close on the rail to be told apart by a pointer. A cluster is
 * drawn once, states its exact count, and opens into its members; it never
 * summarises their states into one. */
export interface TimelineCluster {
  key: string;
  /** 0–1 along the rail: the position of the cluster's newest member. */
  position: number;
  marks: readonly TimelineMark[];
  oldestMicros: number;
  newestMicros: number;
}

export interface TimelineModel {
  marks: readonly TimelineMark[];
  clusters: readonly TimelineCluster[];
  /** Sources whose authority published no observation time. Typed absence,
   * not a mark at zero. */
  unplaced: readonly { id: EvidenceSourceId; title: string; state: EvidenceState }[];
  /** Extent of the placed marks, or `null` when nothing is placed. */
  extent: { oldestMicros: number; newestMicros: number } | null;
}

/**
 * Where each authority's observation lands on one shared time rail. The rail
 * runs from the oldest published observation to the newest; there is no
 * clock in this model, so `NOW` on the rail means "newest read", never wall
 * time.
 *
 * Most authorities stamp their observation at request time, so a page's reads
 * usually land within milliseconds of one another — one pixel on a rail that
 * spans hours. Marks closer than `clusterEpsilon` (a fraction of the rail,
 * chosen by the caller from the rail's measured width and the minimum hit
 * target) therefore fold into one cluster that opens into its members, so
 * every read stays individually selectable without hit areas overlapping.
 */
export function timelineModel(
  summaries: readonly EvidenceSummary[],
  clusterEpsilon = 0.04,
): TimelineModel {
  const placed = summaries.filter(
    (summary): summary is EvidenceSummary & { observedAtMicros: number } =>
      summary.observedAtMicros != null && Number.isFinite(summary.observedAtMicros),
  );
  const unplaced = summaries
    .filter((summary) => summary.observedAtMicros == null)
    .map((summary) => ({ id: summary.id, title: summary.title, state: summary.state }));
  if (placed.length === 0) return { marks: [], clusters: [], unplaced, extent: null };
  const oldest = Math.min(...placed.map((summary) => summary.observedAtMicros));
  const newest = Math.max(...placed.map((summary) => summary.observedAtMicros));
  const span = newest - oldest;
  const marks: TimelineMark[] = [...placed]
    .sort((left, right) => left.observedAtMicros - right.observedAtMicros)
    .map((summary) => ({
      id: summary.id,
      title: summary.title,
      state: summary.state,
      observedAtMicros: summary.observedAtMicros,
      position: span === 0 ? 1 : (summary.observedAtMicros - oldest) / span,
    }));
  const clusters: TimelineCluster[] = [];
  for (const mark of marks) {
    const open = clusters.at(-1);
    const first = open?.marks[0];
    if (open && first && mark.position - first.position < clusterEpsilon) {
      clusters[clusters.length - 1] = {
        ...open,
        position: mark.position,
        marks: [...open.marks, mark],
        newestMicros: mark.observedAtMicros,
      };
    } else {
      clusters.push({
        key: mark.id,
        position: mark.position,
        marks: [mark],
        oldestMicros: mark.observedAtMicros,
        newestMicros: mark.observedAtMicros,
      });
    }
  }
  return { marks, clusters, unplaced, extent: { oldestMicros: oldest, newestMicros: newest } };
}

/** A relative label for a tick, measured back from the newest read. */
export function relativeTickLabel(deltaMicros: number): string {
  const seconds = Math.round(deltaMicros / 1_000_000);
  if (seconds <= 0) return 'newest';
  if (seconds < 90) return `-${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 90) return `-${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `-${hours}h`;
  return `-${Math.round(hours / 24)}d`;
}
