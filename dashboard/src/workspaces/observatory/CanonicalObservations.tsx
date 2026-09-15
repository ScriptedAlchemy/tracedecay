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
import { ObservatoryReadModelV1Schema, type ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { CanonicalReadModelSection } from '../../ui/CanonicalReadModelSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';

export function CanonicalObservations() {
  return (
    <CanonicalReadModelSection<ObservatoryReadModelV1>
      title="Canonical observations"
      blurb={
        'event flow, terminal failures, telemetry drops, and retrieval-feedback latency' +
        ' — the same Plan 26 read model the CLI and MCP serve'
      }
      queryKey={['observatory', 'canonical']}
      url="/api/observatory"
      schema={ObservatoryReadModelV1Schema}
      refetchInterval={30_000}
      loadingDetail="requesting canonical observations"
      className="border-b border-edge-subtle"
      metrics={(model) => model.metrics}
      emptyLabel="the read model carried no measurements — this is a payload with no metrics, not a set of zeroes"
      horizonAttributes={(model) => ({
        'data-observations-current': model.current ? 'true' : 'false',
      })}
      horizonFields={(model) => <HorizonFields model={model} />}
    />
  );
}

/**
 * The window the numbers above were measured over, plus the scope they were
 * authorized for and the watermark they were read at.
 *
 * `current` is printed as the server's own word rather than as a green dot: a
 * read that is not current is not broken, it is a read of an older frontier,
 * and the distinction only survives if it is stated.
 *
 * A horizon `since_micros` of 0 means the composer was asked for an open-ended
 * window, not for 1970, so it says so rather than printing an epoch date that
 * reads as a real measurement boundary.
 */
function HorizonFields({ model }: { model: ObservatoryReadModelV1 }) {
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });
  return (
    <>
      <Field label="horizon">
        {stamp(model.horizon.since_micros)} → {stamp(model.horizon.until_micros)}
      </Field>
      <Field label="observed at">{stamp(model.observed_at_micros)}</Field>
      <Field label="authorized scope">{model.authorized_scope_ref}</Field>
      <Field label="frontier">
        {model.current ? 'current' : 'not current'} · watermark {model.watermark}
      </Field>
    </>
  );
}
