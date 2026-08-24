/**
 * FACT TRUST HISTORY — why one fact's trust is where it is.
 *
 * The fact inspector already shows a trust gauge and a helpful/unhelpful split,
 * and both are terminal figures: they say where the score landed, not how. This
 * drilldown reads `/fact/{id}/trust-history`, the append-only feedback audit,
 * and prints the events that moved it.
 *
 * Two truths this surface exists to keep:
 *
 *   - An event whose detail is `redacted` HAS a detail that was withheld,
 *     and one whose availability is `unknown` never recorded whether it had one.
 *     Both render as their own state chip rather than as a blank note, which is
 *     what a plain optional `note` would have made of them. These are the first
 *     two supplied-backend uses of the `redacted` and `unknown` chips in a
 *     workspace, and they are the daemon's words, not this dashboard's.
 */
import { PayloadBoundary } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { Readout } from '../../ui/instrument.tsx';
import {
  useFactTrustHistory,
  type TrustHistoryEvent,
  type TrustHistoryPayload,
} from '../../data/query/memory.ts';
import { formatUtcMicros, trustDetailState, trustHistoryReading } from './memoryModel.ts';

export function FactTrustHistory({ factId }: { factId: string | null }) {
  const history = useFactTrustHistory(factId);
  if (factId == null) return null;
  return (
    <section className="flex flex-col gap-2 border-t border-edge-subtle pt-3" aria-label="Trust history">
      <h3 className="td-legend">trust history</h3>
      <PayloadBoundary title="Trust history" pending={history.isPending} result={history.data}>
        {(data) => <TrustHistoryBody data={data} />}
      </PayloadBoundary>
    </section>
  );
}

function TrustHistoryBody({ data }: { data: TrustHistoryPayload }) {
  // The handler emits `error: ""` on success and a sentence on failure; a
  // failure that still parsed must not render as an audit with no events.
  if (data.error !== '') {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        the trust audit could not be read: {data.error}
      </p>
    );
  }
  const reading = trustHistoryReading(data);
  const complete = data.completeness === 'complete';
  return (
    <div className="flex flex-col gap-2">
      {complete ? null : (
        <p role="status" className="text-3xs leading-relaxed text-state-partial">
          This is a partial history window of at most {data.limit.toLocaleString()}{' '}
          events. A continuation is available; all tallies below describe this
          window only.
        </p>
      )}
      {reading.count === 0 ? (
        <p className="text-2xs leading-relaxed text-text-secondary">
          {complete
            ? 'no feedback has ever been recorded against this fact — its trust is the score it was stored with, not a score anything has moved'
            : 'no feedback events were returned in this partial window'}
        </p>
      ) : (
        <>
          <dl className="grid grid-cols-3 gap-x-3 gap-y-1">
            <div>
              <dt className="td-legend">events</dt>
              <dd className="td-value text-2xs" data-cell="numeric">
                {reading.count.toLocaleString()}
              </dd>
            </div>
            <div>
              <dt className="td-legend">helpful</dt>
              <dd className="td-value text-2xs" data-cell="numeric">
                {reading.helpful.toLocaleString()}
              </dd>
            </div>
            <div>
              <dt className="td-legend">unhelpful</dt>
              <dd className="td-value text-2xs" data-cell="numeric">
                {reading.unhelpful.toLocaleString()}
              </dd>
            </div>
          </dl>
          <div className="flex items-end gap-3 border-t border-edge-subtle pt-2">
            <Readout
              label={complete ? "opening" : "window opening"}
              size="sm"
              value={reading.opening == null ? '—' : reading.opening.toFixed(3)}
            />
            <Readout
              label={complete ? "net" : "window net"}
              size="sm"
              value={
                reading.net == null
                  ? '—'
                  : `${reading.net >= 0 ? '+' : ''}${reading.net.toFixed(3)}`
              }
            />
            <Readout
              label={complete ? "closing" : "window closing"}
              size="sm"
              value={reading.closing == null ? '—' : reading.closing.toFixed(3)}
            />
          </div>
          {reading.availability.redacted > 0 || reading.availability.unknown > 0 ? (
            <p className="text-3xs leading-relaxed text-text-muted">
              {reading.availability.redacted.toLocaleString()} of{' '}
              {reading.count.toLocaleString()} events had their detail withheld and{' '}
              {reading.availability.unknown.toLocaleString()} never recorded whether they had
              one — {complete
                ? 'the trust arithmetic remains exact.'
                : 'arithmetic is limited to the returned window.'}
            </p>
          ) : null}
          {/* The audit is a list of rows with no focusable content of its own,
            * and it scrolls once a fact has more than a handful of events. A
            * scrollable region has to be keyboard-operable (WCAG 2.1.1), so the
            * list itself takes the tab stop and carries its own accessible
            * name — the name must sit on the node that actually scrolls, not on
            * an ancestor. */}
          <ol
            role="region"
            aria-label="Trust history events"
            tabIndex={0}
            className="flex max-h-64 flex-col gap-1.5 overflow-auto"
          >
            {[...data.trust_history].reverse().map((event) => (
              <TrustEventRow key={event.event_id} event={event} />
            ))}
          </ol>
        </>
      )}
    </div>
  );
}

function TrustEventRow({ event }: { event: TrustHistoryEvent }) {
  const detailState = trustDetailState(event.details_availability);
  const delta = `${event.delta >= 0 ? '+' : ''}${event.delta.toFixed(3)}`;
  return (
    <li className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2">
      <p className="flex flex-wrap items-baseline gap-x-2 text-3xs text-text-muted">
        <span className="td-value" data-cell="numeric">
          {formatUtcMicros(event.timestamp)}
        </span>
        <span className="text-text-secondary">{event.action}</span>
        <span className="td-value" data-cell="numeric">
          {event.old_trust.toFixed(3)} → {event.new_trust.toFixed(3)} ({delta})
        </span>
        {event.source ? <span>· {event.source}</span> : null}
      </p>
      {detailState === null ? (
        event.note ? (
          <p className="text-2xs leading-relaxed text-text-secondary">{event.note}</p>
        ) : null
      ) : (
        <StateChip
          kind={detailState}
          detail={
            detailState === 'redacted'
              ? 'feedback detail withheld'
              : 'detail state never recorded'
          }
        />
      )}
    </li>
  );
}
