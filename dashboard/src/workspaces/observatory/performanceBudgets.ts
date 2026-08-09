/** Plan 26 performance dimensions from the canonical Observatory projection. */
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { readMetric, type PlanDimension, type ReadAnchors } from './planDimension.ts';

const NO_PERCENTILE_PROJECTION =
  'the canonical Observatory projection has no operation-latency evidence for this horizon';

/** Why no span stage reaches this surface. */
const NO_SPAN_PROJECTION =
  'the canonical Observatory projection has no span evidence for this horizon';

/** Why no resource figure reaches this surface. */
const NO_RESOURCE_PROJECTION =
  'the canonical Observatory projection has no resource evidence for this horizon';

/** Why no no-progress outcome reaches this surface. */
const NO_PROGRESS_PROJECTION =
  'the canonical Observatory projection has no no-progress evidence for this horizon';

/** Why no accepted budget revision reaches this surface. */
const NO_BUDGET_REVISION =
  'no accepted performance budget is published; the descriptor revision on a measured card is the projector definition, not an accepted budget';

/** One band of the view. Bands are the plan's own grouping of the sentence, not
 * a taxonomy invented here. */
export interface BudgetBand {
  marker: string;
  label: string;
  dimensions: PlanDimension[];
}

/**
 * The percentile band.
 *
 * Each metric may still arrive with `value: null` and the projector's own
 * reason; that unmeasured state is preserved rather than rendered as zero.
 */
export function latencyDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'latency_p50',
      label: 'latency p50',
      requirement: 'p50 with support and interval over an explicit eligible population',
      reading: readMetric(model.metrics, 'operation_latency_p50', NO_PERCENTILE_PROJECTION),
    },
    {
      id: 'latency_p95',
      label: 'latency p95',
      requirement: 'p95 with support and interval over an explicit eligible population',
      reading: readMetric(model.metrics, 'operation_latency_p95', NO_PERCENTILE_PROJECTION),
    },
    {
      id: 'latency_p99',
      label: 'latency p99',
      requirement: 'p99 with support and interval over an explicit eligible population',
      reading: readMetric(model.metrics, 'operation_latency_p99', NO_PERCENTILE_PROJECTION),
    },
    {
      id: 'revocation_propagation_p95',
      label: 'revocation propagation p95',
      requirement: 'propagation p95 over revocation observations, its own population',
      reading: readMetric(
        model.metrics,
        'feedback_revocation_propagation_p95',
        NO_PERCENTILE_PROJECTION,
      ),
    },
  ];
}

/** The span band: queue, both locks, and provider negotiation. Named
 * individually because the plan names them individually and a single "spans"
 * card would hide which of four is unavailable. */
export function spanDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  const span = (id: string, label: string, stage: string, metric: string): PlanDimension => ({
    id,
    label,
    requirement: `closed SpanStageV1 ${stage} duration with support and interval`,
    reading: readMetric(model.metrics, metric, NO_SPAN_PROJECTION),
  });
  return [
    span('queue_span', 'queue span', 'queue', 'queue_span_p95'),
    span('store_lock_span', 'store-lock span', 'store-lock', 'store_lock_span_p95'),
    span('index_lock_span', 'index-lock span', 'index-lock', 'index_lock_span_p95'),
    span(
      'provider_negotiation_span',
      'provider-negotiation span',
      'provider-negotiation',
      'provider_negotiation_span_p95',
    ),
  ];
}

/** The resource band: RSS, CPU, and I/O, kept as three dimensions because Plan
 * 26 keeps resources, latency, and tokens as separate axes and never collapses
 * them into one score. */
export function resourceDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  const resource = (
    id: string,
    label: string,
    requirement: string,
    metric: string,
  ): PlanDimension => ({
    id,
    label,
    requirement,
    reading: readMetric(model.metrics, metric, NO_RESOURCE_PROJECTION),
  });
  return [
    resource(
      'process_rss',
      'process-tree RSS',
      'baseline/peak/steady process-tree RSS and PSS, with container high-water evidence named separately',
      'process_rss_peak',
    ),
    resource(
      'cpu_time',
      'CPU time',
      'user and system CPU over the same eligible population',
      'cpu_time_total',
    ),
    resource(
      'io_amplification',
      'I/O amplification',
      'temporary and database bytes with read/write amplification',
      'io_amplification',
    ),
  ];
}

/** The outcome band: no-progress escalation and the accepted budget revision a
 * reading would have to be judged against. */
export function outcomeDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'no_progress_outcomes',
      label: 'no-progress outcomes',
      requirement:
        'stalled frontier, escalation action, and terminal/effect-reconciliation outcome; a heartbeat never advances the frontier',
      reading: readMetric(model.metrics, 'no_progress_outcomes', NO_PROGRESS_PROJECTION),
    },
    {
      id: 'accepted_budget_revision',
      label: 'accepted budget revision',
      requirement: 'the accepted budget revision each figure above is judged against',
      reading: readMetric(model.metrics, 'accepted_budget_revision', NO_BUDGET_REVISION),
    },
  ];
}

/** Every band, in the order the plan sentence names them. */
export function performanceBudgetBands(model: ObservatoryReadModelV1): BudgetBand[] {
  return [
    { marker: 'latency', label: 'Latency percentiles', dimensions: latencyDimensions(model) },
    { marker: 'spans', label: 'Queue, lock, and provider spans', dimensions: spanDimensions(model) },
    { marker: 'resources', label: 'RSS, CPU, and I/O', dimensions: resourceDimensions(model) },
    { marker: 'outcomes', label: 'No-progress and budget revision', dimensions: outcomeDimensions(model) },
  ];
}

/** The anchors every card falls back to: the scope the read was authorized for,
 * the watermark it was taken at, and its window. */
export function budgetAnchors(model: ObservatoryReadModelV1): ReadAnchors {
  return {
    authorizedScopeRef: model.authorized_scope_ref,
    watermark: model.watermark,
    horizon: model.horizon,
  };
}

/** Totals for the view header. Both numbers are stated because "2 measured"
 * alone would not say out of how many requirements. */
export function budgetCoverage(bands: readonly BudgetBand[]): {
  measured: number;
  required: number;
  unprojected: number;
} {
  const dimensions = bands.flatMap((band) => band.dimensions);
  return {
    measured: dimensions.filter((dimension) => dimension.reading.kind === 'measured').length,
    required: dimensions.length,
    unprojected: dimensions.filter((dimension) => dimension.reading.kind === 'unpublished').length,
  };
}
