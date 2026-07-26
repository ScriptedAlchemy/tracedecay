import type { LiveActivityPulse } from '../../data/sse/connect.ts';

/**
 * Turns the bounded live-activity ring (see `connect.ts`'s
 * `MAX_ACTIVITY_PULSES`) into the figures a compact instrument readout can
 * print. Pure and framework-free, so it is testable without React or a live
 * stream.
 *
 * Every number here is counted straight off pulses that actually landed —
 * never smoothed, extrapolated, or defaulted to something friendlier than the
 * truth.
 */

/** The trailing window the rate is measured over. A rate has to be measured
 * against a window ending NOW; measured across the ring's own span instead, a
 * burst of 64 events would keep reporting its original rate forever after the
 * stream fell silent, because the ring never empties on its own. Anchoring to
 * the present is what lets the figure fall to zero when activity stops. */
export const RATE_WINDOW_MS = 60_000;

export interface ActivityFamilyCount {
  family: string;
  label: string;
  count: number;
}

export interface ActivitySummary {
  /** Pulses held in the ring right now. */
  total: number;
  /** Families ranked by count over the whole ring, busiest first; ties break
   * alphabetically so the ranking is stable across renders of one ring. */
  families: readonly ActivityFamilyCount[];
  /** Count of the busiest family, so a caller can rail the rest against it.
   * `null` on an empty ring — there is no scale to rank against. */
  peak: number | null;
  /** Wall-clock span the ring covers, or `null` when fewer than two pulses
   * give it no span at all. */
  spanMs: number | null;
  /** Events observed in the trailing {@link RATE_WINDOW_MS}, expressed per
   * minute. Falls to zero when the stream goes quiet, which is the point. */
  ratePerMinute: number;
}

export function summarizeActivity(
  pulses: readonly LiveActivityPulse[],
  now: number,
): ActivitySummary {
  if (pulses.length === 0) {
    return { total: 0, families: [], peak: null, spanMs: null, ratePerMinute: 0 };
  }
  const counts = new Map<string, number>();
  for (const pulse of pulses) counts.set(pulse.family, (counts.get(pulse.family) ?? 0) + 1);
  const families = [...counts.entries()]
    .map(([family, count]) => ({ family, label: familyLabel(family), count }))
    .sort((a, b) => b.count - a.count || a.family.localeCompare(b.family));
  const oldest = pulses[0]!.at;
  const newest = pulses[pulses.length - 1]!.at;
  const span = newest - oldest;
  const since = now - RATE_WINDOW_MS;
  let recent = 0;
  for (const pulse of pulses) if (pulse.at > since) recent += 1;
  return {
    total: pulses.length,
    families,
    peak: families[0]?.count ?? null,
    spanMs: span > 0 ? span : null,
    ratePerMinute: (recent / RATE_WINDOW_MS) * 60_000,
  };
}

/** Human label for an event family. Falls back to de-slugging an unknown
 * family rather than hiding it — a family the dashboard has never been taught
 * to name is still real activity and must stay legible. */
export function familyLabel(family: string): string {
  if (family === 'heartbeat') return 'heartbeat';
  if (family === 'project_registry_changed') return 'project registry';
  if (family === 'storage_telemetry_invalidated') return 'storage telemetry';
  if (family.startsWith('code_index')) return 'code index';
  return family.replace(/_/g, ' ');
}

/**
 * A duration in the dashboard's short relative vocabulary. `null` in (no
 * event observed yet, or a non-finite/negative delta) renders as an em dash
 * rather than a fabricated "0s".
 *
 * The unit is in the name on purpose. Loom's `tracks.ts` formats the same
 * vocabulary from epoch SECONDS, so while both were called `formatDuration`
 * there were two functions of one name whose inputs differ by 1000x — and
 * reaching for the wrong one prints a wrong duration rather than failing,
 * which is a falsified value on screen. Loom's is now
 * `formatDurationSeconds`; callers of either have to name the unit they hold,
 * and `tracks.test.ts` fails if a bare `formatDuration` reappears.
 */
export function formatDurationMs(ms: number | null): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1_000) return '<1s';
  if (ms < 60_000) return `${Math.round(ms / 1_000)}s`;
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m`;
  return `${Math.round(ms / 3_600_000)}h`;
}

/**
 * How often an age readout has to be recomputed to stay true, given the age it
 * is currently showing. A displayed age is the one figure on the panel that
 * goes stale by itself: nothing re-renders while the stream is silent, which
 * is exactly when the number is changing. So the panel re-reads the clock —
 * not to animate anything, but because "last event 4s ago" pinned on screen
 * ten minutes after the fact is a lie, and a silent stream is precisely the
 * case the viewer most needs told accurately.
 *
 * The cadence tracks the resolution actually on display, so an hour-old
 * reading is not recomputed sixty times a minute to no effect.
 */
export function ageTickIntervalMs(ageMs: number): number {
  if (ageMs < 60_000) return 1_000;
  if (ageMs < 3_600_000) return 15_000;
  return 60_000;
}
