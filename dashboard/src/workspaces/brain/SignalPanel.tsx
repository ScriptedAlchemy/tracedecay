import { useEffect, useReducer, useState } from 'react';
import type { LiveActivityPulse, SseConnectionState } from '../../data/sse/connect.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Meter } from '../../ui/instrument.tsx';
import {
  ageTickIntervalMs,
  formatDurationMs,
  summarizeActivity,
  RATE_WINDOW_MS,
} from './activitySummary.ts';

/** How the connection state maps onto the sixteen-state domain taxonomy. Kept
 * beside the panel that renders it because the mapping is the whole honesty
 * claim: `offline` is a genuinely dead EventSource and is NEVER inferred from
 * "nothing lately", which is only a quiet system. */
const CONNECTION_STATE: Record<
  SseConnectionState,
  { kind: DomainStateKind; detail: string; sentence: string }
> = {
  live: {
    kind: 'ready',
    detail: 'event stream open',
    sentence: 'Connected. Counts cover only the retained pulse window, not the full stream.',
  },
  connecting: {
    kind: 'loading',
    detail: 'opening event stream',
    sentence: 'Reconnecting. Figures below are the last ones received.',
  },
  offline: {
    kind: 'offline',
    detail: 'event stream closed',
    sentence: 'Disconnected — the readings below are frozen, not idle.',
  },
};

/**
 * The Brain's live-signal readout: connection honesty first, then what the
 * pulse ring actually holds.
 *
 * Two states this panel exists to keep apart:
 *
 *   idle     the stream is open and nothing is happening. The chip reads
 *            READY, the rate is a truthful zero, and the age of the last event
 *            climbs steadily — which is information, and true.
 *   offline  the stream is dead. The chip carries a different icon, label and
 *            token, the sentence says so in words, and the rate stops being
 *            reported at all, because nothing is measuring it.
 *
 * The distinction never rests on colour, and never on the absence of activity.
 *
 * Nothing here is on a timer except the age clock below, and that exists to
 * keep a printed number true rather than to make anything move.
 */
export function SignalPanel({
  pulses,
  sseState,
  lastEventAt,
  onInspectProject,
}: {
  pulses: readonly LiveActivityPulse[];
  sseState: SseConnectionState;
  lastEventAt: number | null;
  onInspectProject?: (projectId: string | null) => void;
}) {
  const now = useClockWhileAging(lastEventAt);
  const [selectedEventId, setSelectedEventId] = useState<string | null>(null);
  const selectedEvent = pulses.find((pulse) => pulse.eventId === selectedEventId);
  const summary = summarizeActivity(pulses, now);
  const connection = CONNECTION_STATE[sseState];
  const ageMs = lastEventAt == null ? null : Math.max(0, now - lastEventAt);
  const offline = sseState === 'offline';
  const peak = summary.peak;
  return (
    <div className="pointer-events-auto flex max-w-full items-stretch">
      <span aria-hidden className="w-2 border-y border-l border-accent/40" />
      <div className="flex min-w-0 flex-col gap-2 bg-surface-0/75 px-3.5 py-2 backdrop-blur-sm">
        <StateChip kind={connection.kind} detail={connection.detail} />
        <p className="max-w-52 text-3xs leading-snug text-text-secondary">
          {connection.sentence}
        </p>
        {/* Each term precedes its description in the DOM, and
          * `flex-col-reverse` puts the figure back above its legend on screen.
          * The readout looks identical; a screen reader now hears a name before
          * the number it belongs to instead of after it. */}
        <dl className="flex flex-wrap items-end gap-x-5 gap-y-2">
          <div className="flex flex-col-reverse gap-1">
            <dt className="td-legend">
              {offline ? 'rate · not measured' : `retained · last ${RATE_WINDOW_MS / 1000}s`}
            </dt>
            <dd className="td-value text-xs text-text-primary" data-cell="numeric">
              {/* A rate is a claim that something is being measured right now.
               * With the stream down nothing is, so the figure is withheld
               * rather than decayed toward a comfortable zero that would look
               * exactly like a healthy quiet system. */}
              {offline ? '—' : summary.ratePerMinute.toFixed(0)}
            </dd>
          </div>
          <div className="flex flex-col-reverse gap-1">
            <dt className="td-legend">since last event</dt>
            <dd className="td-value text-xs text-text-primary" data-cell="numeric">
              {formatDurationMs(ageMs)}
            </dd>
          </div>
          <div className="flex flex-col-reverse gap-1">
            <dt className="td-legend">
              held{summary.spanMs != null ? ` · ${formatDurationMs(summary.spanMs)} span` : ''}
            </dt>
            <dd className="td-value text-xs text-text-primary" data-cell="numeric">
              {summary.total}
            </dd>
          </div>
        </dl>
        {peak != null ? (
          <dl className="flex flex-col gap-1">
            {summary.families.slice(0, 4).map((entry) => (
              <div key={entry.family} className="flex items-center gap-2">
                <dt className="td-legend w-24 shrink-0 truncate">{entry.label}</dt>
                <Meter fraction={entry.count / peak} className="min-w-8 flex-1" />
                <dd
                  className="td-value w-5 shrink-0 text-right text-2xs text-text-primary"
                  data-cell="numeric"
                >
                  {entry.count}
                </dd>
              </div>
            ))}
          </dl>
        ) : offline ? null : (
          // Offline already says everything in its sentence; repeating "no
          // events" under it reads as a second, redundant apology.
          <p className="td-legend">no events observed yet</p>
        )}
        <details className="max-w-sm text-2xs">
          <summary className="cursor-pointer">Inspect admitted events ({pulses.length} retained)</summary>
          <div className="mt-2 max-h-56 overflow-auto">
            {pulses.length === 0 ? <p>No admitted event is retained. Transcript target unavailable.</p> : (
              <ol aria-label="Retained admitted events" className="space-y-1">
                {[...pulses].reverse().map((pulse) => <li key={pulse.eventId}>
                  <button type="button" className="td-hit w-full break-all border border-edge-subtle p-1 text-left" aria-pressed={selectedEventId === pulse.eventId} onClick={() => {
                    setSelectedEventId(pulse.eventId);
                    onInspectProject?.(pulse.projectId);
                  }}>
                    {pulse.family} · {pulse.eventId}
                  </button>
                </li>)}
              </ol>
            )}
            {selectedEvent ? <section aria-label="Admitted event evidence" className="mt-2 border border-accent/50 p-2">
              <dl className="space-y-1 [&>dd]:break-all">
                <dt>Event ID</dt><dd>{selectedEvent.eventId}</dd>
                <dt>Observed at (Unix microseconds)</dt><dd>{selectedEvent.observationTime}</dd>
                <dt>Family</dt><dd>{selectedEvent.family}</dd>
                <dt>Touched project</dt><dd>{selectedEvent.projectId ?? 'unavailable: event is unscoped'}</dd>
                <dt>Stream cursor</dt><dd>{selectedEvent.streamId}</dd>
              </dl>
              <p className="mt-2">Exact admitted envelope. Transcript target unavailable: this event carries no session/message identity.</p>
            </section> : selectedEventId ? <p className="mt-2">Selected event is no longer retained in the bounded stream window.</p> : null}
          </div>
        </details>
      </div>
      <span aria-hidden className="w-2 border-y border-r border-accent/40" />
    </div>
  );
}

/**
 * A clock that runs only while there is a real elapsed age to keep true.
 *
 * This is not motion and not a heartbeat: nothing here invents activity or
 * draws a frame. It re-reads `Date.now()` so that the printed age of a real,
 * already-received event does not sit frozen at the value it had when the last
 * render happened — which is precisely what would happen during silence, since
 * silence produces no renders. A stale "4s" pinned on screen ten minutes later
 * would be a lie about exactly the condition the viewer most needs to see.
 *
 * With no event ever received there is no age to age, and no timer runs at
 * all. The cadence backs off as the reading coarsens, so an idle dashboard is
 * doing a few integer comparisons a minute and nothing more.
 */
function useClockWhileAging(lastEventAt: number | null): number {
  const [, tick] = useReducer((count: number) => count + 1, 0);
  const now = Date.now();
  const interval = lastEventAt == null ? null : ageTickIntervalMs(now - lastEventAt);
  useEffect(() => {
    if (interval == null) return;
    const id = setInterval(tick, interval);
    return () => clearInterval(id);
  }, [interval]);
  return now;
}
