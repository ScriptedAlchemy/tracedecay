import type {
  ExecutionTopologyDimensionV1,
  ExecutionTopologyMetricsV1,
} from '../../contracts/generated.ts';
import { formatMicrosUtc } from '../../ui/format.ts';
import { Field } from '../../ui/instrument.tsx';
import { MetricPlate } from '../../ui/MetricPlate.tsx';
import { coverageSentence, humanizeMetric } from '../../ui/metricModel.ts';
import { ReadSection, type ReadState } from '../../ui/ReadSection.tsx';
import { useWorkTopologyMetrics } from '../work/workViewsQueries.ts';
import type { WorkResult } from '../work/workApi.ts';

/**
 * Plan 26's execution-topology accounting belongs on Costs beside the token
 * and pricing observations: duplicate work, reruns, delivery loss, and
 * blocked intervals are accounting facts as well as operating facts. The
 * daemon projects these descriptors once; this surface only presents them.
 */
export function TopologyMetricsCosts() {
  const topology = useWorkTopologyMetrics(true);
  return (
    <ReadSection
      title="Execution topology accounting"
      chrome="panel"
      blurb="canonical execution descriptors with their denominators, coverage, delivery evidence, and unavailable states"
      state={topologyReadState(topology.isPending, topology.data)}
    >
      {(model) => <TopologyMetricsReading model={model} />}
    </ReadSection>
  );
}

function topologyReadState(
  pending: boolean,
  result: WorkResult<ExecutionTopologyMetricsV1> | undefined,
): ReadState<ExecutionTopologyMetricsV1> {
  if (pending) {
    return {
      kind: 'blocked',
      state: 'loading',
      detail: 'requesting execution topology accounting',
    };
  }
  if (result === undefined) {
    return { kind: 'blocked', state: 'unknown', detail: 'no topology response recorded' };
  }
  if (result.outcome === 'refused') {
    return { kind: 'blocked', state: result.state, detail: result.detail };
  }
  return { kind: 'ready', value: result.value };
}

function TopologyMetricsReading({ model }: { model: ExecutionTopologyMetricsV1 }) {
  return (
    <div className="flex min-w-0 flex-col gap-4 px-4 pb-4 pt-3">
      <dl className="grid gap-x-5 gap-y-3 border border-edge-subtle bg-surface-1 p-3 text-3xs sm:grid-cols-2 xl:grid-cols-4">
        <Field label="horizon">
          {formatMicrosUtc(model.horizon.since_micros)} → {formatMicrosUtc(model.horizon.until_micros)}
        </Field>
        <Field label="observed at">{formatMicrosUtc(model.observed_at_micros)}</Field>
        <Field label="authorized scope">{model.authorized_scope_ref}</Field>
        <Field label="frontier">
          {model.current ? 'current' : 'not current'} · watermark {model.watermark}
        </Field>
        <Field label="read coverage">{coverageSentence(model.coverage)}</Field>
        <Field label="envelope delivery">{emissionCoverageSentence(model)}</Field>
        <Field label="GitHub stacked-PR capability">
          {githubCapabilitySentence(model)}
        </Field>
        <Field label="GitHub capability coverage">
          {coverageSentence(model.github_stack_capability.coverage)}
        </Field>
      </dl>

      {model.measurements.length === 0 ? (
        <p className="text-2xs text-text-secondary">
          the canonical read returned no topology descriptors
        </p>
      ) : (
        <ul className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
          {model.measurements.map((measurement, index) => (
            <MetricPlate
              key={measurementKey(measurement.dimensions, measurement.value.metric, index)}
              metric={measurement.value}
              annotation={<Dimensions dimensions={measurement.dimensions} />}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

/** Read-model fields stay distinct: null coverage is an unreported count, not
 * a zero; no aggregate, rate, or loss estimate is made in the browser. */
function emissionCoverageSentence(model: ExecutionTopologyMetricsV1): string {
  const coverage = model.emission_coverage;
  return [
    reportedCount(coverage.emitted, 'emitted'),
    reportedCount(coverage.delayed, 'delayed'),
    reportedCount(coverage.dropped, 'dropped'),
    reportedCount(coverage.sampled_events, 'sampled envelopes'),
  ].join(' · ');
}

function githubCapabilitySentence(model: ExecutionTopologyMetricsV1): string {
  const capability = model.github_stack_capability;
  const state =
    capability.capability == null ? 'not reported' : humanizeMetric(capability.capability);
  const unavailable =
    capability.unavailable == null ? null : humanizeMetric(capability.unavailable);
  return [
    state,
    `standard Git fallback ${booleanReading(capability.standard_git_fallback_available)}`,
    `other-forge fallback ${booleanReading(capability.other_forge_fallback_available)}`,
    unavailable == null ? null : `unavailable: ${unavailable}`,
  ]
    .filter((part): part is string => part !== null)
    .join(' · ');
}

function reportedCount(value: number | null, label: string): string {
  return value == null ? `${label} not reported` : `${value.toLocaleString()} ${label}`;
}

function booleanReading(value: boolean | null): string {
  if (value == null) return 'not reported';
  return value ? 'available' : 'unavailable';
}

function Dimensions({ dimensions }: { dimensions: ExecutionTopologyDimensionV1[] }) {
  if (dimensions.length === 0) {
    return <span>unpartitioned descriptor</span>;
  }
  return (
    <span>
      {dimensions
        .map((dimension) => `${humanizeMetric(dimension.dimension)} · ${humanizeMetric(dimension.value)}`)
        .join(' · ')}
    </span>
  );
}

function measurementKey(
  dimensions: ExecutionTopologyDimensionV1[],
  metric: string,
  index: number,
): string {
  return `${metric}:${dimensions
    .map((dimension) => `${dimension.dimension}:${dimension.value}`)
    .join('|')}:${index}`;
}
