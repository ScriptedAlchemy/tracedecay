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
 * matters. Prices are recorded at ingest, so the projector emits a cost metric
 * with a null value and `pricing_revision_unavailable` whenever the turns it
 * counted were never priced. That is a real accounting state, and the plate
 * renders it as one instead of as `$0.00`.
 */
import { useQuery } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { CostsReadModelSchema, type CostsReadModel } from '../../contracts/wire.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { EnvelopeTruth, OmissionReasons, ReadModelState } from '../../ui/EnvelopeTruth.tsx';
import { MetricGroups } from '../../ui/MetricPlate.tsx';
import { StateChip } from '../../ui/StateChip';

export function CanonicalCosts() {
  const scope = useScope((s) => s.scope);
  const costs = useQuery({
    queryKey: ['costs', 'canonical', scopeKey(scope)],
    queryFn: () => fetchEnvelope(scopedUrl(scope, '/api/costs'), CostsReadModelSchema),
    refetchInterval: 60_000,
  });

  return (
    <section className="border-t border-edge-subtle" aria-label="Canonical cost observations">
      <h2 className="px-4 pt-4 text-sm font-semibold tracking-tight">
        Canonical cost observations
      </h2>
      <p className="px-4 pt-0.5 text-2xs text-text-muted">
        usage and estimated cost with their eligible populations, coverage, and pricing
        provenance — the same Plan 26 read model the CLI and MCP serve
      </p>
      {costs.isPending ? (
        <ReadModelState kind="loading" detail="requesting canonical cost observations" />
      ) : costs.data === undefined ? (
        <ReadModelState kind="unknown" detail="no response recorded" />
      ) : (
        <CostsReadModelBody
          result={costs.data}
          refreshing={costs.isFetching}
          onRefresh={() => void costs.refetch()}
        />
      )}
    </section>
  );
}

function CostsReadModelBody({
  result,
  refreshing,
  onRefresh,
}: {
  result: EnvelopeResult<CostsReadModel>;
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
      <div className="flex flex-col gap-4 px-4 py-3">
        <MetricGroups
          metrics={[...model.usage, ...model.estimated_cost]}
          emptyLabel="the read model carried no cost measurements — this is a payload with no metrics, not a zero bill"
        />
        <LatencyBreakdown />
      </div>
    </>
  );
}

function HorizonLine({ model }: { model: CostsReadModel }) {
  return (
    <dl
      className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
      data-costs-current={model.current ? 'true' : 'false'}
    >
      <Field label="horizon">
        {formatMicros(model.horizon.since_micros)} → {formatMicros(model.horizon.until_micros)}
      </Field>
      <Field label="observed at">{formatMicros(model.observed_at_micros)}</Field>
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
    </dl>
  );
}

/**
 * Latency, stated as absent rather than drawn.
 *
 * Plan 11 asks Costs for a latency breakdown alongside tokens and cost. The
 * canonical cost projection has no latency measurement in it: `CostsReadModelV1`
 * carries `usage` and `estimated_cost` only, and neither the accounting-turn
 * ledger nor the savings ledger records a per-call duration for the projector
 * to measure. There is therefore no provider or model latency anywhere behind
 * this surface.
 *
 * The one latency TraceDecay does measure is retrieval-side — the Plan 37
 * feedback percentiles — and it answers a different question about a different
 * population. Borrowing it to fill this panel would be a fabricated provider
 * latency, so this says where the real measurement lives instead and leaves the
 * gap named.
 */
function LatencyBreakdown() {
  return (
    <section
      className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      aria-label="Latency breakdown"
      data-costs-latency="unavailable"
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">latency breakdown</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <StateChip kind="unsupported" detail="no provider latency is measured" />
      <p className="text-3xs leading-snug text-text-muted">
        The canonical cost projection measures usage and estimated cost only. Neither the
        accounting-turn ledger nor the savings ledger records a per-call duration, so no
        provider or model latency exists to break down here. Retrieval-side latency is
        measured, and is reported by Observatory as{' '}
        <span className="td-value">feedback latency p95</span> over its own population — it is
        not a provider timing and is not shown here as one.
      </p>
    </section>
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

/** The costs projector is asked for an all-time window, which reaches the wire
 * as `since_micros: 0`. That is an unbounded horizon, not January 1970. */
function formatMicros(micros: number): string {
  if (micros === 0) return 'unbounded';
  return new Date(Math.floor(micros / 1000)).toISOString();
}
