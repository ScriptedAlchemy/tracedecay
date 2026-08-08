/**
 * `performance-budgets` — the Plan 26 required view (§"Required product views",
 * `performance-budgets` shows "p50/p95/p99 with support and intervals,
 * queue/lock/provider spans, RSS/CPU/I/O, no-progress outcomes, and accepted
 * budget revision").
 *
 * WHAT IS ACTUALLY BEHIND THIS SURFACE
 *
 * One landed read route publishes canonical Plan 26 measurements to this
 * dashboard: `GET /api/observatory` → `ObservatoryReadModelV1`. Of the budget
 * dimensions the plan lists, that projection carries exactly two — the Plan 37
 * feedback-system percentiles `feedback_latency_p95` and
 * `feedback_revocation_propagation_p95`, each with its own denominator,
 * coverage counts, descriptor revision, and watermark.
 *
 * The rest of the list is *recorded server-side and not projected*. The domain
 * families exist and the producers write them —
 * `OperationResourceObservedV1` carries p50/p95/p99-eligible scheduled-arrival
 * and service latency, the closed `SpanStageV1` queue/store-lock/index-lock/
 * provider-negotiation spans, process-tree RSS/PSS, user/system CPU, and
 * temporary/database read/write amplification; `NoProgressObservedV1` carries
 * the run-deadline identity, stalled frontier, escalation, and terminal
 * outcome — but no read route projects any of them into a read model, so
 * nothing on the wire reaches this file.
 *
 * That absence is the view. Plan 26 is explicit that "unsupported or
 * under-floor metrics render unavailable rather than zero", so each of those
 * dimensions is stated as an unpublished requirement with the exact reason,
 * and none of them renders a 0, an empty bar, or a blank row. Wiring a
 * fabricated span or a browser-side percentile here would be the specific
 * failure the plan's truthful-aggregation section exists to prevent.
 *
 * The p50 and p99 cards are separate from p95 for the same reason: the wire
 * publishes one percentile, and printing p95 under a "p50/p95/p99" heading
 * would assert three readings from one measurement.
 */
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { readMetric, type PlanDimension, type ReadAnchors } from './planDimension.ts';

/** Why no percentile other than the feedback p95 reaches this surface. */
const NO_PERCENTILE_PROJECTION =
  'OperationResourceObservedV1 records p50/p95/p99-eligible scheduled-arrival and service latency, but no landed read route projects those percentiles';

/** Why no span stage reaches this surface. */
const NO_SPAN_PROJECTION =
  'the closed SpanStageV1 span set is recorded on OperationResourceObservedV1, but no landed read route projects span durations';

/** Why no resource figure reaches this surface. */
const NO_RESOURCE_PROJECTION =
  'process-tree RSS/PSS, separately named container/cgroup high-water evidence, CPU, and read/write amplification are recorded on OperationResourceObservedV1, but no landed read route projects them';

/** Why no no-progress outcome reaches this surface. */
const NO_PROGRESS_PROJECTION =
  'NoProgressObservedV1 is recorded with its pinned run-deadline identity, stalled frontier, escalation, and terminal outcome, but no landed read route projects it';

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
 * `feedback_latency_p95` and `feedback_revocation_propagation_p95` are the two
 * real measurements. Both may still arrive with `value: null` and the
 * projector's own reason — an unmeasured percentile — which is a different
 * state from an unprojected one and renders as such.
 */
export function latencyDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'latency_p50',
      label: 'latency p50',
      requirement: 'p50 with support and interval over an explicit eligible population',
      reading: { kind: 'unpublished', reason: NO_PERCENTILE_PROJECTION },
    },
    {
      id: 'latency_p95',
      label: 'latency p95',
      requirement: 'p95 with support and interval over an explicit eligible population',
      reading: readMetric(model.metrics, 'feedback_latency_p95', NO_PERCENTILE_PROJECTION),
    },
    {
      id: 'latency_p99',
      label: 'latency p99',
      requirement: 'p99 with support and interval over an explicit eligible population',
      reading: { kind: 'unpublished', reason: NO_PERCENTILE_PROJECTION },
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
export function spanDimensions(): PlanDimension[] {
  const span = (id: string, label: string, stage: string): PlanDimension => ({
    id,
    label,
    requirement: `closed SpanStageV1 ${stage} duration with support and interval`,
    reading: { kind: 'unpublished', reason: NO_SPAN_PROJECTION },
  });
  return [
    span('queue_span', 'queue span', 'queue'),
    span('store_lock_span', 'store-lock span', 'store-lock'),
    span('index_lock_span', 'index-lock span', 'index-lock'),
    span('provider_negotiation_span', 'provider-negotiation span', 'provider-negotiation'),
  ];
}

/** The resource band: RSS, CPU, and I/O, kept as three dimensions because Plan
 * 26 keeps resources, latency, and tokens as separate axes and never collapses
 * them into one score. */
export function resourceDimensions(): PlanDimension[] {
  const resource = (id: string, label: string, requirement: string): PlanDimension => ({
    id,
    label,
    requirement,
    reading: { kind: 'unpublished', reason: NO_RESOURCE_PROJECTION },
  });
  return [
    resource(
      'process_rss',
      'process-tree RSS',
      'baseline/peak/steady process-tree RSS and PSS, with container high-water evidence named separately',
    ),
    resource('cpu_time', 'CPU time', 'user and system CPU over the same eligible population'),
    resource(
      'io_amplification',
      'I/O amplification',
      'temporary and database bytes with read/write amplification',
    ),
  ];
}

/** The outcome band: no-progress escalation and the accepted budget revision a
 * reading would have to be judged against. */
export function outcomeDimensions(): PlanDimension[] {
  return [
    {
      id: 'no_progress_outcomes',
      label: 'no-progress outcomes',
      requirement:
        'stalled frontier, escalation action, and terminal/effect-reconciliation outcome; a heartbeat never advances the frontier',
      reading: { kind: 'unpublished', reason: NO_PROGRESS_PROJECTION },
    },
    {
      id: 'accepted_budget_revision',
      label: 'accepted budget revision',
      requirement: 'the accepted budget revision each figure above is judged against',
      reading: { kind: 'unpublished', reason: NO_BUDGET_REVISION },
    },
  ];
}

/** Every band, in the order the plan sentence names them. */
export function performanceBudgetBands(model: ObservatoryReadModelV1): BudgetBand[] {
  return [
    { marker: 'latency', label: 'Latency percentiles', dimensions: latencyDimensions(model) },
    { marker: 'spans', label: 'Queue, lock, and provider spans', dimensions: spanDimensions() },
    { marker: 'resources', label: 'RSS, CPU, and I/O', dimensions: resourceDimensions() },
    { marker: 'outcomes', label: 'No-progress and budget revision', dimensions: outcomeDimensions() },
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
