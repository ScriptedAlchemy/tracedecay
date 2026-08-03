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
import { CostsReadModelV1Schema, type CostsReadModelV1 } from '../../contracts/generated.ts';
import { CanonicalReadModelSection } from '../../ui/CanonicalReadModelSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { StateChip } from '../../ui/StateChip';

export function CanonicalCosts() {
  return (
    <CanonicalReadModelSection<CostsReadModelV1>
      title="Canonical cost observations"
      blurb={
        'usage and estimated cost with their eligible populations, coverage, and pricing' +
        ' provenance — the same Plan 26 read model the CLI and MCP serve'
      }
      queryKey={['costs', 'canonical']}
      url="/api/costs"
      schema={CostsReadModelV1Schema}
      refetchInterval={60_000}
      loadingDetail="requesting canonical cost observations"
      className="border-t border-edge-subtle"
      metrics={(model) => [...model.usage, ...model.estimated_cost]}
      emptyLabel="the read model carried no cost measurements — this is a payload with no metrics, not a zero bill"
      horizonAttributes={(model) => ({ 'data-costs-current': model.current ? 'true' : 'false' })}
      horizonFields={(model) => <HorizonFields model={model} />}
      footer={<LatencyBreakdown />}
    />
  );
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
