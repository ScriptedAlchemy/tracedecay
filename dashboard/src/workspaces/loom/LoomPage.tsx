import { z } from 'zod';
import { Waypoints } from 'lucide-react';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { formatCount } from '../../ui/format.ts';
import { TrackCanvas } from './TrackCanvas.tsx';
import {
  formatDuration,
  formatMoment,
  peakConcurrency,
  windowOf,
  type LoomTrack,
} from './tracks.ts';

const BASE = '/api/plugins/hermes-lcm';

const OverviewPayload = z
  .object({ latest_sessions: z.array(AnyObject).optional() })
  .passthrough();

/** Loom: session activity as provider tracks over a shared knowledge-time
 * axis — the canvas track engine (Perfetto model) that hook invocations,
 * automation runs, and agent turns plug into as further span sources.
 *
 * The weave is the surface. Counts ride on a hairline instrument strip rather
 * than occupying tiles above the only thing worth looking at. */
export function LoomPage() {
  const overview = useLegacy(['lcm', 'overview'], `${BASE}/overview`, OverviewPayload);

  return (
    <LegacyBoundary title="Loom" pending={overview.isPending} result={overview.data}>
      {(data) => {
        const tracks = tracksFrom(data.latest_sessions ?? []);
        const spanCount = tracks.reduce((sum, track) => sum + track.spans.length, 0);
        const messages = tracks.reduce(
          (sum, track) => sum + track.spans.reduce((n, span) => n + span.weight, 0),
          0,
        );
        const extent = windowOf(tracks);
        const busiest = [...tracks].sort(
          (a, b) => peakConcurrency(b) - peakConcurrency(a),
        )[0];

        return (
          <div className="flex h-full min-h-0 flex-col">
            <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Loom</h1>
              <span className="text-2xs text-text-muted">
                {tracks.length} provider {tracks.length === 1 ? 'track' : 'tracks'} ·{' '}
                {spanCount} sessions · knowledge time
              </span>
            </header>

            {tracks.length === 0 ? (
              <EmptyWeave />
            ) : (
              <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-4 [scrollbar-gutter:stable]">
                <InstrumentStrip
                  items={[
                    { label: 'tracks', value: String(tracks.length) },
                    { label: 'sessions', value: spanCount.toLocaleString() },
                    { label: 'messages', value: formatCount(messages) },
                    {
                      label: 'extent',
                      value: extent ? formatDuration(extent.end - extent.start) : '—',
                    },
                    {
                      label: 'peak overlap',
                      value: busiest ? `×${peakConcurrency(busiest)}` : '—',
                    },
                    {
                      label: 'latest',
                      value: extent ? formatMoment(extent.end) : '—',
                    },
                  ]}
                />
                <TrackCanvas tracks={tracks} />
              </div>
            )}

            <p className="border-t border-edge-subtle px-4 py-2 text-2xs text-text-muted">
              One span source is wired: LCM sessions. Hook invocations, automation runs
              and agent turns attach to these same tracks as their read models begin
              reporting timestamps — until then this weave shows sessions and nothing
              else.
            </p>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}

/** Group timestamped session rows into one track per provider. Rows without a
 * usable last timestamp are dropped rather than guessed at. */
function tracksFrom(rows: ReadonlyArray<Record<string, unknown>>): LoomTrack[] {
  const byProvider = new Map<string, LoomTrack>();
  for (const row of rows) {
    const provider = String(row['provider'] ?? row['source'] ?? 'unknown');
    const last = Number(row['last_timestamp'] ?? 0);
    if (!Number.isFinite(last) || last <= 0) continue;
    const first = Number(row['first_timestamp'] ?? 0);
    const messages = Number(row['message_count'] ?? 1);
    const track = byProvider.get(provider) ?? { id: provider, label: provider, spans: [] };
    track.spans.push({
      id: String(row['session_id'] ?? row['id'] ?? `${provider}-${last}`),
      start: first > 0 && first < last ? first : last,
      end: last,
      label: String(row['session_id'] ?? `${provider} session`),
      weight: Number.isFinite(messages) && messages > 0 ? messages : 1,
    });
    byProvider.set(provider, track);
  }
  return [...byProvider.values()].sort((a, b) => a.label.localeCompare(b.label));
}

/** Corner-bracketed hairline readout: the counts, in the space of one row. */
function InstrumentStrip({
  items,
}: {
  items: ReadonlyArray<{ label: string; value: string }>;
}) {
  return (
    <div className="flex select-none items-stretch self-start">
      <span aria-hidden className="w-2 border-y border-l border-accent/40" />
      <dl className="flex flex-wrap items-center gap-x-5 gap-y-1 px-3 py-1.5">
        {items.map((item) => (
          <div key={item.label} className="flex items-baseline gap-1.5">
            <dd className="tabular text-sm font-semibold leading-none text-text-primary">
              {item.value}
            </dd>
            <dt className="text-2xs uppercase tracking-wider text-text-muted">
              {item.label}
            </dt>
          </div>
        ))}
      </dl>
      <span aria-hidden className="w-2 border-y border-r border-accent/40" />
    </div>
  );
}

/** Composed empty state: the frame stays, so an empty weave reads as an
 * answered question rather than a broken page. */
function EmptyWeave() {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-8">
      <div className="flex max-w-sm flex-col items-center gap-3 text-center">
        <span
          aria-hidden
          className="flex size-10 items-center justify-center rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 text-text-muted"
        >
          <Waypoints size={18} />
        </span>
        <h2 className="text-sm font-semibold tracking-tight">No timestamped spans</h2>
        <p className="text-xs leading-relaxed text-text-muted">
          The session store answered, but no row carried a usable timestamp — so
          there is nothing to place on the knowledge-time axis.{' '}
          <span className="text-text-secondary">
            Sessions appear here as soon as the store records their first and last
            message times.
          </span>
        </p>
      </div>
    </div>
  );
}
