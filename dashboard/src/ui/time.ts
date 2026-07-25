/**
 * Time language for the operational surfaces.
 *
 * Both Delivery and Automations answer "when did this last happen" and "how
 * often does this happen" from epoch seconds. These helpers keep that language
 * identical on both pages so a reader learns it once, and they never
 * extrapolate: an absent timestamp stays absent (`null`), it never becomes a
 * zero, a "now", or a guess.
 */

/** Ordered recency tiers. Position in the order IS the signal, so the meaning
 * survives without colour. */
export type FreshnessTier = 'live' | 'recent' | 'aging' | 'dormant';

export const FRESHNESS_TIERS: readonly FreshnessTier[] = [
  'dormant',
  'aging',
  'recent',
  'live',
];

/** Bucket boundaries, stated once so the legend and the per-row meter can
 * never disagree. */
export const FRESHNESS_BOUNDS: Readonly<Record<FreshnessTier, string>> = {
  live: 'under a day',
  recent: 'under a week',
  aging: 'under a month',
  dormant: 'a month or more',
};

const MINUTE = 60;
const HOUR = 3600;
const DAY = 86_400;
const MONTH = 30 * DAY;

export function freshnessTier(ageSecs: number): FreshnessTier {
  if (ageSecs < DAY) return 'live';
  if (ageSecs < 7 * DAY) return 'recent';
  if (ageSecs < MONTH) return 'aging';
  return 'dormant';
}

/** 1..4 — how many steps of the freshness meter are filled. */
export function freshnessSteps(tier: FreshnessTier): number {
  return FRESHNESS_TIERS.indexOf(tier) + 1;
}

/** "15m ago" / "6d ago" / "1mo ago". Returns null for an absent timestamp so
 * callers must render the absence explicitly. */
export function relativeAge(
  epochSecs: number | null | undefined,
  nowSecs: number,
): string | null {
  if (epochSecs == null || !Number.isFinite(epochSecs)) return null;
  const delta = Math.max(0, nowSecs - epochSecs);
  if (delta < MINUTE) return 'just now';
  if (delta < HOUR) return `${Math.floor(delta / MINUTE)}m ago`;
  if (delta < DAY) return `${Math.floor(delta / HOUR)}h ago`;
  if (delta < MONTH) return `${Math.floor(delta / DAY)}d ago`;
  return `${Math.floor(delta / MONTH)}mo ago`;
}

/** "15m" / "6h" / "2d" — a bare span, for intervals and cooldowns. */
export function formatSpan(secs: number | null | undefined): string | null {
  if (secs == null || !Number.isFinite(secs) || secs < 0) return null;
  if (secs < MINUTE) return `${Math.round(secs)}s`;
  if (secs < HOUR) return `${Math.round(secs / MINUTE)}m`;
  if (secs < DAY) {
    const hours = secs / HOUR;
    return `${Number.isInteger(hours) ? hours : hours.toFixed(1)}h`;
  }
  const days = secs / DAY;
  return `${Number.isInteger(days) ? days : days.toFixed(1)}d`;
}
