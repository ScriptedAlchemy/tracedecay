/**
 * OBSERVATORY — `GET /api/observatory` (Plan 26 canonical observations).
 *
 * This is the read model the CLI, MCP, and this dashboard all share: the same
 * `ObservatoryReadModelV1` bytes, projected once by
 * `application::observability::observatory_read_model` and never re-derived per
 * transport. It carries the event-flow measurements (events admitted, terminal
 * failures, proved telemetry drops) alongside the Plan 37 feedback-system
 * quality measurements, which is where the latency percentiles live.
 *
 * What this surface must not do, and does not:
 *
 *   - it never computes a health grade from these numbers. `current` is the
 *     server's own statement about whether the read is complete, and the domain
 *     state on the envelope is the server's too;
 *   - it never renders a metric with no value as zero. The composer emits
 *     `value: null` plus a reason for every metric it could not complete, and
 *     `MetricPlate` prints that reason where the figure would be;
 *   - it never merges the two producing sources. An observability-envelope
 *     count and a feedback-observation ratio answer different questions over
 *     different populations, and averaging or co-plotting them would invent a
 *     relationship the wire does not assert.
 */
import { useQuery } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { ObservatoryReadModelSchema, type ObservatoryReadModel } from '../../contracts/wire.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { EnvelopeTruth, OmissionReasons, ReadModelState } from '../../ui/EnvelopeTruth.tsx';
import { MetricGroups } from '../../ui/MetricPlate.tsx';

export function CanonicalObservations() {
  const scope = useScope((s) => s.scope);
  const observations = useQuery({
    queryKey: ['observatory', 'canonical', scopeKey(scope)],
    queryFn: () => fetchEnvelope(scopedUrl(scope, '/api/observatory'), ObservatoryReadModelSchema),
    refetchInterval: 30_000,
  });

  return (
    <section className="border-b border-edge-subtle" aria-label="Canonical observations">
      <h2 className="px-4 pt-4 text-sm font-semibold tracking-tight">Canonical observations</h2>
      <p className="px-4 pt-0.5 text-2xs text-text-muted">
        event flow, terminal failures, telemetry drops, and retrieval-feedback latency — the same
        Plan 26 read model the CLI and MCP serve
      </p>
      {observations.isPending ? (
        <ReadModelState kind="loading" detail="requesting canonical observations" />
      ) : observations.data === undefined ? (
        <ReadModelState kind="unknown" detail="no response recorded" />
      ) : (
        <ObservationsReadModel
          result={observations.data}
          refreshing={observations.isFetching}
          onRefresh={() => void observations.refetch()}
        />
      )}
    </section>
  );
}

function ObservationsReadModel({
  result,
  refreshing,
  onRefresh,
}: {
  result: EnvelopeResult<ObservatoryReadModel>;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  if (result.outcome === 'transport') {
    return <ReadModelState kind={result.state} detail={result.detail ?? 'daemon unreachable'} />;
  }
  const { envelope } = result;
  const model = envelope.payload;
  return (
    <>
      <EnvelopeTruth
        state={envelope.domain_state}
        coverage={envelope.coverage}
        freshness={envelope.freshness}
        legalActions={envelope.legal_actions}
        authorization={envelope.authorization}
        refreshing={refreshing}
        onRefresh={onRefresh}
      />
      <OmissionReasons coverage={envelope.coverage} />
      <HorizonLine model={model} />
      <div className="px-4 py-3">
        <MetricGroups
          metrics={model.metrics}
          emptyLabel="the read model carried no measurements — this is a payload with no metrics, not a set of zeroes"
        />
      </div>
    </>
  );
}

/**
 * The window the numbers above were measured over, plus the scope they were
 * authorized for and the watermark they were read at.
 *
 * `current` is printed as the server's own word rather than as a green dot: a
 * read that is not current is not broken, it is a read of an older frontier,
 * and the distinction only survives if it is stated.
 */
function HorizonLine({ model }: { model: ObservatoryReadModel }) {
  return (
    <dl
      className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
      data-observations-current={model.current ? 'true' : 'false'}
    >
      <Field label="horizon">
        {formatMicros(model.horizon.since_micros)} → {formatMicros(model.horizon.until_micros)}
      </Field>
      <Field label="observed at">{formatMicros(model.observed_at_micros)}</Field>
      <Field label="authorized scope">{model.authorized_scope_ref}</Field>
      <Field label="frontier">
        {model.current ? 'current' : 'not current'} · watermark {model.watermark}
      </Field>
    </dl>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="uppercase tracking-[0.08em] text-text-muted">{label}</dt>
      <dd className="min-w-0 break-words text-text-secondary tabular">{children}</dd>
    </div>
  );
}

/**
 * A horizon `since_micros` of 0 means the composer was asked for an open-ended
 * window, not for 1970. It says so rather than printing an epoch date that
 * reads as a real measurement boundary.
 */
function formatMicros(micros: number): string {
  if (micros === 0) return 'unbounded';
  return new Date(Math.floor(micros / 1000)).toISOString();
}
