/**
 * COSTS — `GET /api/costs` (Plan 26 canonical cost observations).
 *
 * The savings overview above it is the legacy rollup: real figures, but shaped
 * for a dashboard rather than for accounting. This is the projection the CLI
 * and MCP also serve, and it carries what the rollup cannot — the eligible
 * population behind each figure, how much of that population was actually
 * observed, what the value was priced against, and the reason a measurement is
 * missing when it is.
 *
 * The `provider_cost` measurement is normally the clearest example of why that
 * matters. Provider usage is priced at read time, so the projector emits a
 * null cost whenever exact provider/model pricing is unavailable. That is a
 * real accounting state, and the plate
 * renders it as one instead of as `$0.00`.
 */
import {
  CostsReadModelV1Schema,
  type CostsReadModelV1,
  type MetricValueV1,
  type ProviderLatencyReadModelV1,
} from '../../contracts/generated.ts';
import { CanonicalReadModelSection } from '../../ui/CanonicalReadModelSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { MetricPlate } from '../../ui/MetricPlate.tsx';

const COSTS_QUERY_KEY = ['costs', 'canonical'] as const;
const COSTS_URL = '/api/costs';

export function CanonicalCosts() {
  return (
    <CanonicalReadModelSection<CostsReadModelV1>
      title="Canonical cost observations"
      blurb={
        'usage, estimated cost, and provider latency with their eligible populations,' +
        ' coverage, and provenance — the same Plan 26 read model the CLI and MCP serve'
      }
      queryKey={COSTS_QUERY_KEY}
      url={COSTS_URL}
      schema={CostsReadModelV1Schema}
      refetchInterval={60_000}
      loadingDetail="requesting canonical cost observations"
      className="border-t border-edge-subtle"
      metrics={(model) => [...model.usage, ...model.estimated_cost]}
      emptyLabel="the read model carried no cost measurements — this is a payload with no metrics, not a zero bill"
      horizonAttributes={(model) => ({ 'data-costs-current': model.current ? 'true' : 'false' })}
      horizonFields={(model) => <HorizonFields model={model} />}
      footer={(model) => <ProviderLatencyCohorts cohorts={model.latency} />}
    />
  );
}

/**
 * Provider/model identity belongs to the cohort, not to its percentile cells.
 * Keep each cohort around its own canonical metric list from the model already
 * decoded by `CanonicalReadModelSection`.
 */
function ProviderLatencyCohorts({
  cohorts,
}: {
  cohorts: ProviderLatencyReadModelV1[];
}) {
  return (
    <section className="flex min-w-0 flex-col gap-3" aria-label="Provider latency cohorts">
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">provider latency cohorts</h3>
        <span aria-hidden className="td-rule" />
        <span className="shrink-0 text-3xs text-text-muted tabular">
          {cohorts.length.toLocaleString()} reported
        </span>
      </div>
      {cohorts.length === 0 ? (
        <p className="text-2xs text-text-secondary">
          the canonical read returned no provider latency cohorts
        </p>
      ) : (
        cohorts.map((cohort, index) => {
          const cohortKey = providerLatencyCohortKey(cohort, index);
          return (
            <section
              key={cohortKey}
              className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-0 p-3"
              data-provider-latency-cohort={cohortKey}
            >
              <div className="flex min-w-0 flex-col gap-1">
                <h4 className="text-xs font-semibold text-text-primary">
                  {providerLatencyHeading(cohort)}
                </h4>
                <p className="text-3xs text-text-muted">
                  identity provenance {cohort.identity_provenance.source} ·{' '}
                  {cohort.identity_provenance.source_revision} ·{' '}
                  {cohort.identity_provenance.watermark}
                </p>
              </div>
              <ul className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
                {latencyMetrics(cohort).map((metric) => (
                  <MetricPlate key={`${cohortKey}:${metric.metric}`} metric={metric} />
                ))}
              </ul>
            </section>
          );
        })
      )}
    </section>
  );
}

function latencyMetrics(cohort: ProviderLatencyReadModelV1): MetricValueV1[] {
  return [
    cohort.queue,
    cohort.start,
    cohort.first_progress,
    cohort.service,
    cohort.terminal,
  ].flatMap((distribution) => [distribution.p50, distribution.p95, distribution.p99]);
}

function providerLatencyHeading(cohort: ProviderLatencyReadModelV1): string {
  const identity =
    cohort.provider === null && cohort.model === null
      ? 'provider/model unavailable'
      : `${cohort.provider ?? 'provider unavailable'} · ${cohort.model ?? 'model unavailable'}`;
  return cohort.identity_unavailable_reason === null
    ? identity
    : `${identity} · ${cohort.identity_unavailable_reason}`;
}

function providerLatencyCohortKey(
  cohort: ProviderLatencyReadModelV1,
  index: number,
): string {
  return [
    cohort.provider ?? 'provider-unavailable',
    cohort.model ?? 'model-unavailable',
    cohort.identity_unavailable_reason ?? 'identity-known',
    cohort.identity_provenance.source,
    cohort.identity_provenance.watermark,
    index,
  ].join(':');
}

/** The costs projector is asked for an all-time window, which reaches the wire
 * as `since_micros: 0`. That is an unbounded horizon, not January 1970. */
function HorizonFields({ model }: { model: CostsReadModelV1 }) {
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });
  return (
    <>
      <Field label="horizon">
        {stamp(model.horizon.since_micros)} → {stamp(model.horizon.until_micros)}
      </Field>
      <Field label="observed at">{stamp(model.observed_at_micros)}</Field>
      <Field label="authorized scope">{model.authorized_scope_ref}</Field>
      <Field label="pricing revision">
        {/* Not "unpriced" and not a dash on its own: the projector distinguishes
          * "no pricing revision was attached to this read" from a priced read,
          * and a dash would collapse them. */}
        {model.pricing_revision ?? 'none attached to this read'}
      </Field>
      <Field label="frontier">
        {model.current ? 'current' : 'not current'} · watermark {model.watermark}
      </Field>
    </>
  );
}
