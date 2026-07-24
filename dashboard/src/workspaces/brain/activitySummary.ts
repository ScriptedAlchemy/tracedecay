import type { LiveActivityPulse } from '../../data/sse/connect.ts';

/** Turns the bounded live-activity ring (see `connect.ts`'s
 * `MAX_ACTIVITY_PULSES`) into the numbers a compact instrument readout can
 * show. Pure and framework-free — testable without React or a live stream,
 * and honest by construction: every figure here is counted straight off the
 * pulses that actually landed, never smoothed, extrapolated, or defaulted to
 * a friendly placeholder. */

export interface ActivityFamilyCount {
  family: string;
  label: string;
  count: number;
}

export interface ActivitySummary {
  /** Pulses observed in the ring right now. */
  total: number;
  /** Families ranked by recent count, most active first; ties break
   * alphabetically so the ranking is stable across renders of the same
   * ring. */
  families: readonly ActivityFamilyCount[];
  /** Events per minute measured across the ring's own observed span.
   * `null` when the ring cannot support a rate — a rate needs an interval to
   * divide by, and a single pulse (or a burst that landed in the same
   * millisecond) has none. Reporting `null` keeps an unmeasurable rate from
   * masquerading as zero or infinity. */
  ratePerMinute: number | null;
}

export function summarizeActivity(
  pulses: readonly LiveActivityPulse[],
): ActivitySummary {
  if (pulses.length === 0) return { total: 0, families: [], ratePerMinute: null };
  const counts = new Map<string, number>();
  for (const pulse of pulses) counts.set(pulse.family, (counts.get(pulse.family) ?? 0) + 1);
  const families = [...counts.entries()]
    .map(([family, count]) => ({ family, label: familyLabel(family), count }))
    .sort((a, b) => b.count - a.count || a.family.localeCompare(b.family));
  const oldest = pulses[0]!.at;
  const newest = pulses[pulses.length - 1]!.at;
  const spanMs = newest - oldest;
  const ratePerMinute =
    pulses.length > 1 && spanMs > 0 ? ((pulses.length - 1) / spanMs) * 60_000 : null;
  return { total: pulses.length, families, ratePerMinute };
}

/** Human label for an event family. Falls back to de-slugging an unknown
 * family rather than hiding it — a family the dashboard has never named is
 * still real activity and must stay legible, not disappear from the
 * readout. */
export function familyLabel(family: string): string {
  if (family === 'heartbeat') return 'heartbeat';
  if (family === 'project_registry_changed') return 'project registry';
  if (family === 'storage_telemetry_invalidated') return 'storage telemetry';
  if (family.startsWith('code_index')) return 'code index';
  return family.replace(/_/g, ' ');
}

/** Age of the most recent event in the dashboard's short relative-time
 * vocabulary. `null` in (no event observed yet, or a non-finite/negative
 * delta) renders as an em dash rather than a fabricated "0s ago". */
export function formatEventAge(ms: number | null): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1_000) return 'just now';
  if (ms < 60_000) return `${Math.round(ms / 1_000)}s ago`;
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m ago`;
  return `${Math.round(ms / 3_600_000)}h ago`;
}
