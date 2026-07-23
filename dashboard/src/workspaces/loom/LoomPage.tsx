import { z } from 'zod';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { TrackCanvas } from './TrackCanvas.tsx';
import type { LoomTrack } from './tracks.ts';

const BASE = '/api/plugins/hermes-lcm';

const OverviewPayload = z
  .object({ latest_sessions: z.array(AnyObject).optional() })
  .passthrough();

/** Loom: session activity as provider tracks over a shared knowledge-time
 * axis — the canvas track engine (Perfetto model) that hook invocations,
 * automation runs, and agent turns plug into as further span sources. */
export function LoomPage() {
  const overview = useLegacy(['lcm', 'overview'], `${BASE}/overview`, OverviewPayload);

  return (
    <LegacyBoundary title="Loom" pending={overview.isPending} result={overview.data}>
      {(data) => {
        const sessions = data.latest_sessions ?? [];
        const byProvider = new Map<string, LoomTrack>();
        for (const row of sessions) {
          const provider = String(row['provider'] ?? row['source'] ?? 'unknown');
          const last = Number(row['last_timestamp'] ?? 0);
          if (!Number.isFinite(last) || last <= 0) continue;
          const first = Number(row['first_timestamp'] ?? 0);
          const messages = Number(row['message_count'] ?? 1);
          const track = byProvider.get(provider) ?? {
            id: provider,
            label: provider,
            spans: [],
          };
          track.spans.push({
            id: String(row['session_id'] ?? row['id'] ?? `${provider}-${last}`),
            start: first > 0 && first < last ? first : last,
            end: last,
            label: `${String(row['session_id'] ?? '')} · ${messages.toLocaleString()} msgs`,
            weight: messages,
          });
          byProvider.set(provider, track);
        }
        const tracks = [...byProvider.values()].sort((a, b) =>
          a.label.localeCompare(b.label),
        );
        const spanCount = tracks.reduce((sum, track) => sum + track.spans.length, 0);
        return (
          <div className="flex h-full flex-col overflow-auto">
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Loom</h1>
              <span className="text-2xs text-text-muted">
                {tracks.length} providers · {spanCount} sessions · knowledge time
              </span>
            </div>
            <div className="grid grid-cols-2 gap-3 p-4 md:grid-cols-4">
              <StatTile label="providers" value={tracks.length} />
              <StatTile label="sessions in window" value={spanCount} />
            </div>
            <div className="px-4 pb-4">
              <TrackCanvas tracks={tracks} />
            </div>
            <p className="border-t border-edge-subtle px-4 py-2 text-2xs text-text-muted">
              span sources beyond sessions — hook invocations, automation runs,
              agent turns — attach to these tracks as their read models land
            </p>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}
