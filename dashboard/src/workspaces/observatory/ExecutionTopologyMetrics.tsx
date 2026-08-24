import type {
  CoverageStateV1,
  ExecutionTopologyDimensionV1,
  ExecutionTopologyMeasurementV1,
  ExecutionTopologyMetricsV1,
} from '../../contracts/generated.ts';
import { assertNever } from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { Field, Panel } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { humanizeMetric } from '../../ui/metricModel.ts';
import { useWorkTopologyMetrics } from '../work/workViewsQueries.ts';
import { PlanDimensionGrid } from './PlanDimensionCard.tsx';
import {
  planDimensionPresentation,
  type DimensionReading,
  type PlanDimensionPresentation,
  type ReadAnchors,
} from './planDimension.ts';

/**
 * The canonical execution-topology projection mounted in Observatory.
 *
 * This is deliberately a consumer of the Work-owned metrics operation. It
 * does not reconstruct concurrency, fan-out, conflict, integration, blocked,
 * rerun, leak, or delivery facts from other dashboard reads. Every card below
 * is one server-published measurement cell, including cells whose value is a
 * typed absence.
 */
export function ExecutionTopologyMetrics() {
  const read = useWorkTopologyMetrics(true);

  return (
    <Panel
      legend="Execution-topology metrics"
      className="shrink-0"
      bodyClassName="flex flex-col gap-3"
      elevation="well"
    >
      {read.isPending ? (
        <StateChip kind="loading" detail="requesting canonical execution-topology metrics" />
      ) : read.data === undefined ? (
        <StateChip kind="unknown" detail="the metrics read returned no result" />
      ) : read.data.outcome === 'refused' ? (
        <StateChip kind={read.data.state} detail={read.data.detail} />
      ) : (
        <ExecutionTopologyReadModel model={read.data.value} />
      )}
    </Panel>
  );
}

function ExecutionTopologyReadModel({ model }: { model: ExecutionTopologyMetricsV1 }) {
  const groups = measurementGroups(model);
  const state = coverageState(model.coverage.state);
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });

  return (
    <>
      <div className="flex flex-wrap items-center gap-2">
        <StateChip kind={state} detail={`${model.coverage.state} family coverage`} />
        <span className="text-2xs text-text-muted">
          Values, denominators, coverage, and typed omissions are projected by the daemon.
        </span>
      </div>

      <dl
        className="grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
        data-execution-topology-current={model.current ? 'true' : 'false'}
        data-execution-topology-measurements={model.measurements.length}
      >
        <Field label="horizon">
          {stamp(model.horizon.since_micros)} → {stamp(model.horizon.until_micros)}
        </Field>
        <Field label="observed at">{stamp(model.observed_at_micros)}</Field>
        <Field label="authorized scope">{model.authorized_scope_ref}</Field>
        <Field label="frontier">
          {model.current ? 'current' : 'not current'} · watermark {model.watermark}
        </Field>
        <Field label="emission coverage">
          emitted {optionalCount(model.emission_coverage.emitted)} · delayed{' '}
          {optionalCount(model.emission_coverage.delayed)} · dropped ≥{' '}
          {optionalCount(model.emission_coverage.dropped)} · sampled{' '}
          {optionalCount(model.emission_coverage.sampled_events)}
        </Field>
        <Field label="family coverage">
          {model.coverage.observed.toLocaleString()} observed ·{' '}
          {model.coverage.completed.toLocaleString()} completed ·{' '}
          {model.coverage.censored.toLocaleString()} censored ·{' '}
          {model.coverage.unknown.toLocaleString()} unknown
        </Field>
        <Field label="GitHub stack capability">
          {model.github_stack_capability.capability == null
            ? `— · ${model.github_stack_capability.unavailable ?? 'reason not published'}`
            : humanizeMetric(model.github_stack_capability.capability)}
        </Field>
        <Field label="safe drill anchors">
          {model.drill_anchors.length.toLocaleString()} registered local cursors
        </Field>
      </dl>

      {groups.length === 0 ? (
        <StateChip kind="unknown" detail="the projection returned no measurement cells" />
      ) : (
        groups.map((group) => (
          <PlanDimensionGrid
            key={group.metric}
            marker={group.metric}
            label={humanizeMetric(group.metric)}
            dimensions={group.dimensions}
          />
        ))
      )}
    </>
  );
}

function measurementGroups(model: ExecutionTopologyMetricsV1): readonly {
  metric: string;
  dimensions: readonly PlanDimensionPresentation[];
}[] {
  const grouped = new Map<string, ExecutionTopologyMeasurementV1[]>();
  for (const measurement of model.measurements) {
    const cells = grouped.get(measurement.value.metric);
    if (cells === undefined) grouped.set(measurement.value.metric, [measurement]);
    else cells.push(measurement);
  }

  const anchors: ReadAnchors = {
    authorizedScopeRef: model.authorized_scope_ref,
    watermark: model.watermark,
    horizon: model.horizon,
  };
  const drillAnchorSummary = `${model.drill_anchors.length.toLocaleString()} registered local cursors`;

  return [...grouped.entries()].map(([metric, measurements]) => ({
    metric,
    dimensions: measurements.map((measurement, index) => {
      const dimensions = measurement.dimensions.map(dimensionLabel).join(' · ');
      const reading: DimensionReading =
        measurement.value.value == null
          ? {
              kind: 'unmeasured',
              metric: measurement.value,
              reason:
                measurement.unavailable ??
                measurement.value.unavailable_reason ??
                'the projector published no reason',
            }
          : { kind: 'measured', metric: measurement.value };
      const presentation = planDimensionPresentation(
        {
          id: `${metric}:${dimensions || index}`,
          label: dimensions || 'all eligible observations',
          requirement:
            dimensions.length > 0
              ? `Canonical ${humanizeMetric(metric)} cell grouped by ${dimensions}.`
              : `Canonical ${humanizeMetric(metric)} aggregate.`,
          reading,
        },
        anchors,
      );
      return {
        ...presentation,
        anchors: `${presentation.anchors} · ${drillAnchorSummary}`,
      };
    }),
  }));
}

function dimensionLabel(dimension: ExecutionTopologyDimensionV1): string {
  return `${humanizeMetric(dimension.dimension)} ${humanizeMetric(dimension.value)}`;
}

function optionalCount(value: number | null): string {
  return value == null ? '—' : value.toLocaleString();
}

function coverageState(state: CoverageStateV1): DomainStateKind {
  switch (state) {
    case 'known':
      return 'ready';
    case 'partial':
    case 'sampled':
    case 'capped':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'unknown':
      return 'unknown';
    default:
      return assertNever(state);
  }
}
